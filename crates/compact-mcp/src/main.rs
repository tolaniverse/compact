//! `compact-mcp`: give a coding agent a workspace in Compact.
//!
//! Runs as the agent's MCP server over stdio and relays every tool call to the
//! running Compact over its socket (see `compact_agent::wire`). Add it to an
//! agent's MCP config:
//!
//! ```json
//! { "mcpServers": { "compact": {
//!     "command": "/Applications/Compact.app/Contents/MacOS/compact-mcp"
//! } } }
//! ```
//!
//! The workspace is tied to the directory the agent runs in (override with
//! `--project`), and is named after the agent, taken from the MCP client's
//! name (override with `--client`). Compact asks you before the first call
//! from a new agent and project goes through.
//!
//! Connecting is lazy: `initialize` and `tools/list` answer at once, and the
//! socket is opened — launching Compact if it is not running — on the first
//! tool call.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use compact_agent::wire::{Reply, Request};
use compact_agent::{paths, tools};
use serde_json::{json, Value};

/// MCP protocol versions this server can speak, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

struct Args {
    project: Option<String>,
    client: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args { project: None, client: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--project" => a.project = Some(it.next().ok_or("--project needs a directory")?),
            "--client" => a.client = Some(it.next().ok_or("--client needs a name")?),
            "--help" | "-h" => {
                println!("usage: compact-mcp [--project DIR] [--client NAME]\n\nAn MCP server over stdio. See the crate docs for configuration.");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

/// The project as shown and remembered: absolute, with `~` for home.
fn project_of(arg: Option<&str>) -> String {
    let dir = arg.map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let dir = dir.canonicalize().unwrap_or(dir);
    paths::tilde(&dir)
}

/// A friendly name for the workspace from the MCP client's own name.
fn client_name(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "claude-code" | "claude code" => "Claude Code".into(),
        "claude-ai" | "claude" => "Claude".into(),
        "opencode" => "opencode".into(),
        "muse" => "Muse".into(),
        "cursor" | "cursor-vscode" => "Cursor".into(),
        _ if raw.trim().is_empty() => "Agent".into(),
        _ => raw.trim().to_string(),
    }
}

/// The connection to the app, opened on first use.
struct Link {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Link {
    fn send(&mut self, line: &str) -> io::Result<()> {
        self.stream.write_all(line.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()
    }

    /// Wait for the reply to `id`, skipping anything else.
    fn wait(&mut self, id: u64) -> io::Result<Reply> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Compact closed the connection"));
            }
            match Reply::parse(&line) {
                Some((Some(got), reply)) if got == id => return Ok(reply),
                Some((None, Reply::Err(e))) => return Ok(Reply::Err(e)),
                _ => continue,
            }
        }
    }

    fn request(&mut self, line: impl FnOnce(u64) -> String) -> io::Result<Reply> {
        self.next += 1;
        let id = self.next;
        self.send(&line(id))?;
        self.wait(id)
    }
}

/// Connect, launching Compact if nothing is listening.
fn connect() -> Result<UnixStream, String> {
    let path = paths::socket_path().ok_or("cannot find your home directory")?;
    if let Some(why) = paths::socket_path_problem(&path) {
        return Err(why);
    }
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    launch_compact();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
    }
    Err(format!("Compact is not running and did not start (no socket at {})", path.display()))
}

/// Start the Compact this binary was shipped in, or failing that whichever
/// one Launch Services knows.
fn launch_compact() {
    let bundle = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(PathBuf::from));
    let mut cmd = std::process::Command::new("open");
    match bundle {
        Some(app) => cmd.arg("-g").arg(app),
        None => cmd.args(["-g", "-b", "app.compact.Compact"]),
    };
    let _ = cmd.status();
}

struct Session {
    client: String,
    project: String,
    link: Option<Link>,
}

impl Session {
    fn link(&mut self) -> Result<&mut Link, String> {
        if self.link.is_none() {
            let stream = connect()?;
            let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
            let mut link = Link { stream, reader, next: 0 };
            let (client, project) = (self.client.clone(), self.project.clone());
            match link.request(|id| Request::hello(id, &client, &project)).map_err(|e| e.to_string())? {
                Reply::Ok(_) => self.link = Some(link),
                Reply::Err(e) => return Err(e),
            }
        }
        Ok(self.link.as_mut().expect("just connected"))
    }

    /// Run one tool call, reconnecting once if Compact restarted.
    fn call(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        for attempt in 0..2 {
            let link = self.link()?;
            match link.request(|id| Request::call(id, tool, args)) {
                Ok(Reply::Ok(v)) => return Ok(v),
                Ok(Reply::Err(e)) => return Err(e),
                Err(_) if attempt == 0 => self.link = None,
                Err(e) => return Err(e.to_string()),
            }
        }
        unreachable!()
    }
}

/// Turn the app's answer into MCP content: a screenshot becomes an image,
/// anything else readable text.
fn content(v: Value) -> Value {
    if let Some(png) = v.get("png_base64").and_then(Value::as_str) {
        let mut rest = v.clone();
        rest.as_object_mut().map(|o| o.remove("png_base64"));
        return json!([
            {"type": "image", "data": png, "mimeType": "image/png"},
            {"type": "text", "text": rest.to_string()}
        ]);
    }
    let text = match v.get("text").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => serde_json::to_string_pretty(&v).unwrap_or_default(),
    };
    json!([{"type": "text", "text": text}])
}

fn respond(out: &mut impl Write, id: &Value, result: Value) -> io::Result<()> {
    writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result}))?;
    out.flush()
}

fn fail(out: &mut impl Write, id: &Value, code: i64, message: &str) -> io::Result<()> {
    writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))?;
    out.flush()
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("compact-mcp: {e}");
            std::process::exit(2);
        }
    };
    let mut session = Session { client: String::new(), project: project_of(a.project.as_deref()), link: None };
    let stdin = io::stdin();
    let mut out = io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = fail(&mut out, &Value::Null, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // Notifications carry no id and get no answer.
        let Some(id) = msg.get("id").cloned() else { continue };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let written = match method {
            "initialize" => {
                let raw = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("");
                session.client = a.client.clone().unwrap_or_else(|| client_name(raw));
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
                respond(&mut out, &id, json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "compact", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": tools::INSTRUCTIONS,
                }))
            }
            "ping" => respond(&mut out, &id, json!({})),
            "tools/list" => respond(&mut out, &id, tools::list()),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if !tools::ALL.contains(&name) {
                    fail(&mut out, &id, -32602, &format!("unknown tool {name}"))
                } else {
                    if session.client.is_empty() {
                        session.client = a.client.clone().unwrap_or_else(|| client_name(""));
                    }
                    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let result = match session.call(name, &args) {
                        Ok(v) => json!({"content": content(v), "isError": false}),
                        Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                    };
                    respond(&mut out, &id, result)
                }
            }
            _ => fail(&mut out, &id, -32601, &format!("method not found: {method}")),
        };
        if written.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_clients_get_their_proper_names() {
        assert_eq!(client_name("claude-code"), "Claude Code");
        assert_eq!(client_name("opencode"), "opencode");
        assert_eq!(client_name("muse"), "Muse");
        assert_eq!(client_name("my-agent"), "my-agent");
        assert_eq!(client_name(""), "Agent");
    }

    #[test]
    fn screenshots_become_images() {
        let c = content(json!({"png_base64": "AAAA", "tab": "t1"}));
        assert_eq!(c[0]["type"], "image");
        assert_eq!(c[0]["data"], "AAAA");
        assert!(!c[1]["text"].as_str().unwrap().contains("AAAA"));
        let t = content(json!({"text": "page: x"}));
        assert_eq!(t[0]["text"], "page: x");
    }
}
