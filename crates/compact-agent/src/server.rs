//! The app's end of the socket.
//!
//! Accepting and reading happen on background threads; everything else —
//! deciding, answering, touching the app — happens on the main thread, which
//! calls [`Server::poll`] from a timer and [`Server::reply`] when an answer is
//! ready. The threads only move bytes, so no app state is ever shared across
//! them.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::wire::{Reply, Request, MAX_LINE};

/// One connected agent process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ConnId(pub u64);

#[derive(Debug)]
pub enum Event {
    Request(ConnId, Request),
    /// The process went away, or its connection was dropped.
    Closed(ConnId),
}

pub struct Server {
    path: PathBuf,
    events: Receiver<Event>,
    conns: Arc<Mutex<HashMap<ConnId, UnixStream>>>,
}

impl Server {
    /// Listen at `path`, which must not be in use by another running Compact.
    ///
    /// A socket file left behind by a crash is replaced; one that still
    /// answers belongs to a live instance, and taking it over would cut that
    /// instance's agents off, so that is an error instead.
    pub fn start(path: &Path) -> io::Result<Server> {
        if let Some(why) = crate::paths::socket_path_problem(path) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, why));
        }
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "another Compact is already listening"));
            }
            std::fs::remove_file(path)?;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = UnixListener::bind(path)?;
        // Only you. This file's permissions are the whole access check.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

        let (tx, events) = channel();
        let conns: Arc<Mutex<HashMap<ConnId, UnixStream>>> = Arc::default();
        let accepted = conns.clone();
        std::thread::Builder::new().name("compact-agent-accept".into()).spawn(move || {
            let mut next = 0u64;
            for stream in listener.incoming().flatten() {
                next += 1;
                let id = ConnId(next);
                let Ok(writer) = stream.try_clone() else { continue };
                accepted.lock().unwrap().insert(id, writer);
                let tx = tx.clone();
                let conns = accepted.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("compact-agent-{next}"))
                    .spawn(move || read_loop(id, stream, tx, conns));
            }
        })?;
        Ok(Server { path: path.to_path_buf(), events, conns })
    }

    /// Everything that arrived since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Answer request `id` on `conn`. A connection that has gone is ignored:
    /// its `Closed` event is already on its way.
    pub fn reply(&self, conn: ConnId, id: u64, reply: &Reply) {
        self.send(conn, &reply.line(id));
    }

    fn send(&self, conn: ConnId, line: &str) {
        let mut conns = self.conns.lock().unwrap();
        let Some(stream) = conns.get_mut(&conn) else { return };
        let ok = stream.write_all(line.as_bytes()).and_then(|_| stream.write_all(b"\n")).and_then(|_| stream.flush());
        if ok.is_err() {
            conns.remove(&conn);
        }
    }

    /// Hang up on `conn`, e.g. after its access was revoked.
    pub fn drop_conn(&self, conn: ConnId) {
        if let Some(s) = self.conns.lock().unwrap().remove(&conn) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_loop(id: ConnId, stream: UnixStream, tx: Sender<Event>, conns: Arc<Mutex<HashMap<ConnId, UnixStream>>>) {
    // `take` bounds a single line: a peer that never sends a newline cannot
    // make us buffer without limit.
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE as u64 + 1).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) if n > MAX_LINE => {
                reply_direct(&conns, id, &Reply::orphan("line too long"));
                break;
            }
            Ok(_) => {}
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        match Request::parse(line) {
            Ok(req) => {
                if tx.send(Event::Request(id, req)).is_err() {
                    break;
                }
            }
            Err((Some(rid), e)) => reply_direct(&conns, id, &Reply::Err(e).line(rid)),
            Err((None, e)) => reply_direct(&conns, id, &Reply::orphan(&e)),
        }
    }
    conns.lock().unwrap().remove(&id);
    let _ = tx.send(Event::Closed(id));
}

/// Answer a malformed line from the reader thread. Only protocol errors take
/// this path; nothing about the app is known here.
fn reply_direct(conns: &Mutex<HashMap<ConnId, UnixStream>>, id: ConnId, line: &str) {
    if let Some(s) = conns.lock().unwrap().get_mut(&id) {
        let _ = s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Op;
    use std::time::{Duration, Instant};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ca-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("agent.sock")
    }

    fn wait(server: &Server) -> Vec<Event> {
        let t = Instant::now();
        loop {
            let e = server.poll();
            if !e.is_empty() || t.elapsed() > Duration::from_secs(3) {
                return e;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn requests_arrive_and_replies_go_back() {
        let path = scratch("rt");
        let server = Server::start(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only you may connect");

        let mut client = UnixStream::connect(&path).unwrap();
        writeln!(client, "{}", Request::hello(1, "Claude Code", "~/site")).unwrap();
        let events = wait(&server);
        let Event::Request(conn, req) = &events[0] else { panic!("{events:?}") };
        assert_eq!(req.op, Op::Hello { client: "Claude Code".into(), project: "~/site".into() });

        server.reply(*conn, 1, &Reply::Ok(serde_json::json!({"workspace": "w"})));
        let mut line = String::new();
        BufReader::new(&client).read_line(&mut line).unwrap();
        assert_eq!(Reply::parse(&line).unwrap().0, Some(1));

        drop(client);
        assert!(matches!(wait(&server)[0], Event::Closed(_)));
    }

    #[test]
    fn malformed_lines_are_answered_without_reaching_the_app() {
        let path = scratch("bad");
        let server = Server::start(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        (&client).write_all(b"not json\n").unwrap();
        let mut line = String::new();
        BufReader::new(&client).read_line(&mut line).unwrap();
        assert!(line.contains("not JSON"));
        std::thread::sleep(Duration::from_millis(50));
        assert!(server.poll().is_empty());
    }

    #[test]
    fn a_live_socket_is_not_taken_over_but_a_stale_one_is() {
        let path = scratch("live");
        let first = Server::start(&path).unwrap();
        assert_eq!(Server::start(&path).err().map(|e| e.kind()), Some(io::ErrorKind::AddrInUse));
        drop(first);
        // Left behind as a crash would leave it.
        let _ = UnixListener::bind(&path).map(drop);
        assert!(path.exists());
        assert!(Server::start(&path).is_ok());
    }
}
