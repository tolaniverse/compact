//! What an agent workspace's pages said and fetched.
//!
//! A script injected into agent-workspace pages hooks `console`, uncaught
//! errors, `fetch`, `XMLHttpRequest` and failed resource loads, and posts each
//! event as one line. This module parses those lines into bounded per-tab
//! buffers that the agent tools and, later, the developer-tools panels read.
//!
//! It is platform-free on purpose: the parsing and the caps are the parts
//! worth testing, and none of it needs WebKit.
//!
//! **Everything here is the page's own say-so.** The hook runs in the page's
//! world because that is the only place a page's `console.log` can be seen,
//! which also means the page can post whatever it likes. Treat entries as data
//! about the page, never as anything Compact vouches for.

use std::collections::VecDeque;

/// Field separator on the wire: ASCII unit separator, which no URL, log line
/// worth reading or status code contains.
pub const SEP: char = '\u{1f}';

/// Oldest console lines are dropped past this.
pub const CONSOLE_CAP: usize = 1000;
/// Oldest requests are dropped past this.
pub const NETWORK_CAP: usize = 500;
/// A single field is cut to this many characters. A page logging a megabyte
/// of JSON should not be able to make a tool result a megabyte.
pub const FIELD_CAP: usize = 4000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Debug,
    Log,
    Info,
    Warn,
    Error,
    /// Not from the page: the boundary where a new document started.
    Navigation,
}

impl Level {
    fn parse(s: &str) -> Self {
        match s {
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Log,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Log => "log",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Navigation => "navigation",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// Shared with [`NetworkEntry::seq`], so the two can be interleaved in
    /// the order they happened.
    pub seq: u64,
    pub level: Level,
    pub text: String,
    /// `file:line` for uncaught errors; empty otherwise.
    pub source: String,
    /// Stack trace for uncaught errors, when the page had one.
    pub stack: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestKind {
    Fetch,
    Xhr,
    /// An `<img>`, `<script>` or stylesheet that failed to load. Only
    /// failures are seen: the page's JavaScript is never told about
    /// resources that loaded fine.
    Resource,
}

impl RequestKind {
    pub const fn label(self) -> &'static str {
        match self {
            RequestKind::Fetch => "fetch",
            RequestKind::Xhr => "xhr",
            RequestKind::Resource => "resource",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkEntry {
    pub seq: u64,
    pub kind: RequestKind,
    pub method: String,
    pub url: String,
    /// HTTP status; 0 when the request never got a response.
    pub status: u16,
    /// Wall time in milliseconds, when known.
    pub ms: Option<u32>,
    /// Why it failed, when it did without a status.
    pub error: String,
}

impl NetworkEntry {
    /// Failed outright, or answered with a 4xx/5xx.
    pub fn failed(&self) -> bool {
        self.status == 0 || self.status >= 400
    }
}

/// One tab's captured activity.
#[derive(Debug, Default)]
pub struct Capture {
    console: VecDeque<ConsoleEntry>,
    network: VecDeque<NetworkEntry>,
    seq: u64,
}

impl Capture {
    /// Record one line from the page script. Returns false for anything that
    /// is not a well-formed event, which is dropped.
    pub fn ingest(&mut self, line: &str) -> bool {
        let f: Vec<&str> = line.split(SEP).collect();
        let field = |i: usize| f.get(i).map(|s| cut(s)).unwrap_or_default();
        match f[0] {
            "p" if f.len() >= 2 => {
                self.push_console(Level::Navigation, field(1), String::new(), String::new())
            }
            "c" if f.len() >= 3 => {
                self.push_console(Level::parse(f[1]), field(2), String::new(), String::new())
            }
            "e" if f.len() >= 2 => self.push_console(Level::Error, field(1), field(2), field(3)),
            "n" if f.len() >= 6 => {
                let kind = match f[1] {
                    "xhr" => RequestKind::Xhr,
                    _ => RequestKind::Fetch,
                };
                self.push_network(NetworkEntry {
                    seq: 0,
                    kind,
                    method: field(2),
                    url: field(3),
                    status: f[4].parse().unwrap_or(0),
                    ms: f[5].parse().ok(),
                    error: field(6),
                })
            }
            "r" if f.len() >= 3 => self.push_network(NetworkEntry {
                seq: 0,
                kind: RequestKind::Resource,
                method: field(1),
                url: field(2),
                status: 0,
                ms: None,
                error: format!("<{}> failed to load", field(3)),
            }),
            _ => return false,
        }
        true
    }

    fn next(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn push_console(&mut self, level: Level, text: String, source: String, stack: String) {
        let seq = self.next();
        self.console.push_back(ConsoleEntry { seq, level, text, source, stack });
        while self.console.len() > CONSOLE_CAP {
            self.console.pop_front();
        }
    }

    fn push_network(&mut self, mut e: NetworkEntry) {
        e.seq = self.next();
        self.network.push_back(e);
        while self.network.len() > NETWORK_CAP {
            self.network.pop_front();
        }
    }

    /// Console entries after `since` (0 for everything still held).
    pub fn console_since(&self, since: u64) -> impl Iterator<Item = &ConsoleEntry> {
        self.console.iter().filter(move |e| e.seq > since)
    }

    /// Requests after `since` (0 for everything still held).
    pub fn network_since(&self, since: u64) -> impl Iterator<Item = &NetworkEntry> {
        self.network.iter().filter(move |e| e.seq > since)
    }

    /// The newest sequence number handed out, for "since" bookmarks.
    pub fn last_seq(&self) -> u64 {
        self.seq
    }
}

fn cut(s: &str) -> String {
    match s.char_indices().nth(FIELD_CAP) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The page-world hook. Injected at document start into the main frame of
/// agent-workspace pages only, and posting to [`CAPTURE_HANDLER`].
pub const CAPTURE_SCRIPT: &str = r#"(() => {
  const h = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.compactCapture;
  if (!h) return;
  const S = '\u001f';
  const send = (...f) => { try { h.postMessage(f.map(x => String(x == null ? '' : x)).join(S)); } catch (_) {} };
  const fmt = a => {
    if (typeof a === 'string') return a;
    if (a instanceof Error) return a.stack || String(a);
    try { return JSON.stringify(a); } catch (_) { return String(a); }
  };
  const abs = u => { try { return new URL(u, location.href).href; } catch (_) { return String(u); } };
  const ms = t0 => Math.round(performance.now() - t0);
  send('p', location.href);
  for (const level of ['debug', 'log', 'info', 'warn', 'error']) {
    const orig = console[level];
    console[level] = function (...args) {
      send('c', level, args.map(fmt).join(' ').slice(0, 4000));
      return orig.apply(this, args);
    };
  }
  addEventListener('error', e => {
    const t = e.target;
    if (t && t !== window && t.tagName) {
      send('r', 'GET', abs(t.currentSrc || t.src || t.href || ''), t.tagName.toLowerCase());
      return;
    }
    send('e', e.message, (e.filename || '') + ':' + (e.lineno || 0), (e.error && e.error.stack) || '');
  }, true);
  addEventListener('unhandledrejection', e =>
    send('e', 'Unhandled rejection: ' + fmt(e.reason), '', (e.reason && e.reason.stack) || ''));
  const of = window.fetch;
  if (of) {
    window.fetch = function (input, init) {
      const url = abs(typeof input === 'string' ? input : (input && input.url) || String(input));
      const method = String((init && init.method) || (input && input.method) || 'GET').toUpperCase();
      const t0 = performance.now();
      return of.apply(this, arguments).then(
        r => { send('n', 'fetch', method, r.url || url, r.status, ms(t0), ''); return r; },
        err => { send('n', 'fetch', method, url, 0, ms(t0), (err && err.message) || String(err)); throw err; });
    };
  }
  const seen = new WeakMap();
  const XO = XMLHttpRequest.prototype.open, XS = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (m, u) {
    seen.set(this, [String(m).toUpperCase(), abs(u)]);
    return XO.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    const c = seen.get(this) || ['GET', ''];
    const t0 = performance.now();
    this.addEventListener('loadend', () =>
      send('n', 'xhr', c[0], this.responseURL || c[1], this.status, ms(t0), this.status ? '' : 'network error'));
    return XS.apply(this, arguments);
  };
})();"#;

/// The message-handler name [`CAPTURE_SCRIPT`] posts to.
pub const CAPTURE_HANDLER: &str = "compactCapture";

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        fields.join(&SEP.to_string())
    }

    #[test]
    fn console_errors_and_navigations_share_one_timeline() {
        let mut c = Capture::default();
        assert!(c.ingest(&line(&["p", "http://localhost:3000/"])));
        assert!(c.ingest(&line(&["c", "warn", "low on coffee"])));
        assert!(c.ingest(&line(&["n", "fetch", "GET", "http://localhost:3000/api", "500", "12", ""])));
        assert!(c.ingest(&line(&["e", "x is not defined", "app.js:4", "at f (app.js:4)"])));

        let console: Vec<_> = c.console_since(0).collect();
        assert_eq!(console.len(), 3);
        assert_eq!(console[0].level, Level::Navigation);
        assert_eq!(console[1].level, Level::Warn);
        assert_eq!(console[2].level, Level::Error);
        assert_eq!(console[2].source, "app.js:4");
        assert_eq!(console[2].stack, "at f (app.js:4)");

        let net: Vec<_> = c.network_since(0).collect();
        assert_eq!(net[0].seq, 3, "numbered between the console lines around it");
        assert!(net[0].failed());
        assert_eq!(net[0].ms, Some(12));
    }

    #[test]
    fn since_returns_only_what_is_new() {
        let mut c = Capture::default();
        c.ingest(&line(&["c", "log", "one"]));
        let mark = c.last_seq();
        c.ingest(&line(&["c", "log", "two"]));
        let texts: Vec<_> = c.console_since(mark).map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["two"]);
    }

    #[test]
    fn failed_resources_and_requests_are_told_apart_from_successes() {
        let mut c = Capture::default();
        c.ingest(&line(&["r", "GET", "http://localhost:3000/logo.png", "img"]));
        c.ingest(&line(&["n", "xhr", "POST", "http://localhost:3000/save", "0", "3", "network error"]));
        c.ingest(&line(&["n", "fetch", "GET", "http://localhost:3000/ok", "200", "5", ""]));
        let net: Vec<_> = c.network_since(0).collect();
        assert_eq!(net[0].kind, RequestKind::Resource);
        assert_eq!(net[0].error, "<img> failed to load");
        assert_eq!(net[1].kind, RequestKind::Xhr);
        assert!(net[1].failed());
        assert!(!net[2].failed());
    }

    #[test]
    fn malformed_lines_are_dropped() {
        let mut c = Capture::default();
        assert!(!c.ingest(""));
        assert!(!c.ingest("c"));
        assert!(!c.ingest(&line(&["zz", "what"])));
        assert_eq!(c.last_seq(), 0);
    }

    #[test]
    fn buffers_and_fields_are_bounded() {
        let mut c = Capture::default();
        for i in 0..(CONSOLE_CAP + 10) {
            c.ingest(&line(&["c", "log", &i.to_string()]));
        }
        assert_eq!(c.console_since(0).count(), CONSOLE_CAP);
        assert_eq!(c.console_since(0).next().unwrap().text, "10", "oldest dropped");

        let huge = "x".repeat(FIELD_CAP * 3);
        c.ingest(&line(&["c", "log", &huge]));
        let last = c.console_since(0).last().unwrap();
        assert_eq!(last.text.chars().count(), FIELD_CAP + 1, "cut, with an ellipsis");
    }
}
