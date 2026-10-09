//! The line protocol between `compact-mcp` and the app.
//!
//! One JSON object per line, in both directions. Deliberately smaller than
//! MCP: `compact-mcp` speaks MCP to the agent and turns each tool call into a
//! `call` here, so the app never has to track MCP's protocol versions.
//!
//! ```text
//! → {"id":1,"op":"hello","client":"Claude Code","project":"~/proj/site"}
//! ← {"id":1,"ok":{"workspace":"Claude Code's workspace"}}
//! → {"id":2,"op":"call","tool":"open_tab","args":{"url":"localhost:3000"}}
//! ← {"id":2,"ok":{"tab":"t7","url":"http://localhost:3000/","title":"Acme"}}
//! ← {"id":3,"error":"no tab t9 in this workspace; call list_tabs"}
//! ```
//!
//! `hello` must come first on a connection and is answered only once you
//! have allowed the agent, which may take a while.

use serde_json::{json, Value};

/// The longest line either side accepts. A screenshot is the largest thing
/// sent, and a full-page PNG in base64 fits well inside this.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Hello { client: String, project: String },
    Call { tool: String, args: Value },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: u64,
    pub op: Op,
}

impl Request {
    pub fn hello(id: u64, client: &str, project: &str) -> String {
        json!({"id": id, "op": "hello", "client": client, "project": project}).to_string()
    }

    pub fn call(id: u64, tool: &str, args: &Value) -> String {
        json!({"id": id, "op": "call", "tool": tool, "args": args}).to_string()
    }

    /// Parse one line. The error says what was wrong, for the reply.
    pub fn parse(line: &str) -> Result<Request, (Option<u64>, String)> {
        let v: Value = serde_json::from_str(line).map_err(|e| (None, format!("not JSON: {e}")))?;
        let id = v.get("id").and_then(Value::as_u64).ok_or((None, "missing id".to_string()))?;
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let op = match v.get("op").and_then(Value::as_str) {
            Some("hello") => Op::Hello {
                client: text("client").filter(|s| !s.trim().is_empty()).ok_or((Some(id), "hello needs a client".to_string()))?,
                project: text("project").filter(|s| !s.trim().is_empty()).ok_or((Some(id), "hello needs a project".to_string()))?,
            },
            Some("call") => Op::Call {
                tool: text("tool").ok_or((Some(id), "call needs a tool".to_string()))?,
                args: v.get("args").cloned().unwrap_or_else(|| json!({})),
            },
            Some(other) => return Err((Some(id), format!("unknown op {other}"))),
            None => return Err((Some(id), "missing op".to_string())),
        };
        Ok(Request { id, op })
    }
}

/// An answer to one request.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Ok(Value),
    Err(String),
}

impl Reply {
    pub fn line(&self, id: u64) -> String {
        match self {
            Reply::Ok(v) => json!({"id": id, "ok": v}).to_string(),
            Reply::Err(e) => json!({"id": id, "error": e}).to_string(),
        }
    }

    /// An error not tied to a request, such as a line that was not JSON.
    pub fn orphan(error: &str) -> String {
        json!({"id": null, "error": error}).to_string()
    }

    /// Parse a reply line into its id and answer.
    pub fn parse(line: &str) -> Option<(Option<u64>, Reply)> {
        let v: Value = serde_json::from_str(line).ok()?;
        let id = v.get("id").and_then(Value::as_u64);
        if let Some(ok) = v.get("ok") {
            return Some((id, Reply::Ok(ok.clone())));
        }
        let e = v.get("error").and_then(Value::as_str)?;
        Some((id, Reply::Err(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let r = Request::parse(&Request::hello(1, "Claude Code", "~/site")).unwrap();
        assert_eq!(r, Request { id: 1, op: Op::Hello { client: "Claude Code".into(), project: "~/site".into() } });
        let args = json!({"url": "localhost:3000"});
        let r = Request::parse(&Request::call(2, "open_tab", &args)).unwrap();
        assert_eq!(r.op, Op::Call { tool: "open_tab".into(), args });
    }

    #[test]
    fn bad_requests_say_what_is_wrong() {
        assert_eq!(Request::parse("nope").unwrap_err().0, None);
        assert_eq!(Request::parse(r#"{"op":"call"}"#).unwrap_err().1, "missing id");
        assert_eq!(Request::parse(r#"{"id":3,"op":"hello","client":" ","project":"x"}"#).unwrap_err(), (Some(3), "hello needs a client".into()));
        assert_eq!(Request::parse(r#"{"id":4,"op":"dance"}"#).unwrap_err(), (Some(4), "unknown op dance".into()));
    }

    #[test]
    fn replies_round_trip() {
        let ok = Reply::Ok(json!({"tab": "t1"}));
        assert_eq!(Reply::parse(&ok.line(5)), Some((Some(5), ok)));
        let err = Reply::Err("no".into());
        assert_eq!(Reply::parse(&err.line(6)), Some((Some(6), err)));
        assert_eq!(Reply::parse(&Reply::orphan("bad")), Some((None, Reply::Err("bad".into()))));
    }
}
