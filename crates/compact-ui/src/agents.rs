//! The app's side of agent workspaces: answering `compact-mcp`.
//!
//! Requests arrive on the agent socket ([`compact_agent::server`]) and are
//! drained on the 12 Hz load poll, so an answer is at most a frame or two
//! late and nothing here runs off the main thread. Each request becomes a
//! call into the core's agent rules ([`compact_core::agent`]); this module
//! adds only what needs the platform: the approval sheets, waiting for a page
//! to finish loading, and the engine's snapshot and screenshot callbacks.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSApplication, NSModalResponse, NSRequestUserAttentionType};
use objc2_foundation::NSString;
use serde_json::{json, Value};

use compact_agent::server::{ConnId, Event, Server};
use compact_agent::tools;
use compact_agent::wire::{Op, Reply};
use compact_core::agent::{parse_tab_ref, tab_ref, AgentError};
use compact_core::page::PageKind;
use compact_core::tab::TabId;
use compact_engine::capture::Level;

use super::Inner;

/// How long `open_tab`, `navigate` and `reload` wait for a page.
const LOAD_TIMEOUT_MS: u64 = 20_000;
/// A load that reports finished this soon after starting has not started.
const LOAD_SETTLE_MS: u64 = 150;

#[derive(Clone)]
struct Session {
    client: String,
    project: String,
}

/// Something only you can answer, shown one at a time as a sheet.
enum Prompt {
    /// A new agent and project asking for a workspace.
    Hello { conn: ConnId, id: u64, client: String, project: String },
    /// An agent asking to open a site beyond loopback. The call is run again
    /// if you say yes.
    Host { conn: ConnId, id: u64, session: Session, host: String, tool: String, args: Value },
}

/// What to do once a tab has finished loading.
enum After {
    Report,
    Snapshot,
    Screenshot(Option<String>),
}

struct Wait {
    conn: ConnId,
    id: u64,
    tab: TabId,
    since: u64,
    then: After,
}

pub(super) struct AgentHost {
    server: Rc<Server>,
    sessions: HashMap<ConnId, Session>,
    prompts: VecDeque<Prompt>,
    showing: bool,
    waits: Vec<Wait>,
}

impl AgentHost {
    /// Listen on the agent socket, or explain why not. Compact runs fine
    /// without it; agents just cannot connect.
    pub(super) fn start() -> Result<AgentHost, String> {
        let path = compact_agent::paths::socket_path().ok_or("no home directory")?;
        let server = Server::start(&path).map_err(|e| format!("agent socket {}: {e}", path.display()))?;
        Ok(AgentHost {
            server: Rc::new(server),
            sessions: HashMap::new(),
            prompts: VecDeque::new(),
            showing: false,
            waits: Vec::new(),
        })
    }
}

fn err(e: impl std::fmt::Display) -> Reply {
    Reply::Err(e.to_string())
}

impl Inner {
    /// Drain the socket, answer what can be answered, and finish waits.
    pub(super) fn service_agents(&mut self) {
        let Some(host) = self.agents.as_ref() else { return };
        let events = host.server.poll();
        let had_work = !events.is_empty() || !host.waits.is_empty();
        for ev in events {
            match ev {
                Event::Request(conn, req) => self.agent_request(conn, req.id, req.op),
                Event::Closed(conn) => {
                    let h = self.agents.as_mut().expect("checked above");
                    if let Some(s) = h.sessions.remove(&conn) {
                        // Another connection from the same agent keeps it
                        // connected; only the last one leaving counts.
                        let others = h.sessions.values().any(|o| o.client == s.client && o.project == s.project);
                        if !others {
                            self.app.set_agent_connected(&s.client, &s.project, false);
                        }
                    }
                    let h = self.agents.as_mut().expect("checked above");
                    h.waits.retain(|w| w.conn != conn);
                    // A queued question for an agent that left is moot; the
                    // one on screen stays, since your answer still counts.
                    h.prompts.retain(|p| match p {
                        Prompt::Hello { conn: c, .. } | Prompt::Host { conn: c, .. } => *c != conn,
                    });
                }
            }
        }
        self.finish_waits();
        self.show_next_prompt();
        if had_work {
            self.render();
        }
    }

    fn agent_reply(&self, conn: ConnId, id: u64, reply: Reply) {
        if let Some(h) = &self.agents {
            h.server.reply(conn, id, &reply);
        }
    }

    fn agent_request(&mut self, conn: ConnId, id: u64, op: Op) {
        match op {
            Op::Hello { client, project } => {
                let h = self.agents.as_mut().expect("serviced");
                if h.sessions.contains_key(&conn) {
                    return self.agent_reply(conn, id, err("this connection already said hello"));
                }
                if self.app.is_approved(&client, &project) {
                    self.accept_agent(conn, id, &client, &project);
                } else {
                    h.prompts.push_back(Prompt::Hello { conn, id, client, project });
                }
            }
            Op::Call { tool, args } => {
                let Some(session) = self.agents.as_ref().and_then(|h| h.sessions.get(&conn)).cloned() else {
                    return self.agent_reply(conn, id, err("say hello first"));
                };
                if !self.app.is_approved(&session.client, &session.project) {
                    self.agent_reply(conn, id, err("the user revoked this agent's access in Compact"));
                    if let Some(h) = self.agents.as_mut() {
                        h.sessions.remove(&conn);
                        h.server.drop_conn(conn);
                    }
                    return;
                }
                self.agent_call(conn, id, session, &tool, args);
            }
        }
    }

    fn accept_agent(&mut self, conn: ConnId, id: u64, client: &str, project: &str) {
        self.app.approve(client, project);
        let ws = self.app.open_agent_workspace(client, project);
        let name = self.app.workspaces.spaces[ws].name.clone();
        self.app.set_agent_connected(client, project, true);
        if let Some(h) = self.agents.as_mut() {
            h.sessions.insert(conn, Session { client: client.into(), project: project.into() });
        }
        self.agent_reply(conn, id, Reply::Ok(json!({"workspace": name})));
    }

    /// The agent's workspace, reopened if you closed it while it was away.
    fn agent_ws(&mut self, s: &Session) -> usize {
        match self.app.agent_workspace(&s.client, &s.project) {
            Some(ws) => ws,
            None => self.app.open_agent_workspace(&s.client, &s.project),
        }
    }

    /// The tab a call names, or the workspace's active tab.
    fn agent_tab(&self, ws: usize, args: &Value) -> Result<TabId, Reply> {
        match args.get("tab").and_then(Value::as_str) {
            None => Ok(self.app.workspaces.spaces[ws].active),
            Some(s) => parse_tab_ref(s).ok_or_else(|| err(format!("{s:?} is not a tab; tabs look like t12"))),
        }
    }

    fn agent_call(&mut self, conn: ConnId, id: u64, session: Session, tool: &str, args: Value) {
        let ws = self.agent_ws(&session);
        let text = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let tab = match self.agent_tab(ws, &args) {
            Ok(t) => t,
            Err(r) => return self.agent_reply(conn, id, r),
        };
        let wait = |then| Wait { conn, id, tab, since: 0, then };

        let outcome: Result<Option<Wait>, AgentError> = match tool {
            tools::LIST_TABS => {
                let active = self.app.workspaces.spaces[ws].active;
                let tabs: Vec<Value> = self
                    .app
                    .agent_tabs(ws)
                    .unwrap_or_default()
                    .iter()
                    .map(|t| {
                        // A live tab's own word beats the model's, which is
                        // only kept current for the workspace on screen.
                        json!({
                            "tab": tab_ref(t.id),
                            "url": self.engine.url(t.id).unwrap_or_else(|| t.url.clone()),
                            "title": self.engine.title(t.id).unwrap_or_else(|| t.title.clone()),
                            "loaded": t.state.is_live(),
                            "active": t.id == active,
                        })
                    })
                    .collect();
                return self.agent_reply(conn, id, Reply::Ok(json!({"tabs": tabs})));
            }
            tools::OPEN_TAB | tools::NAVIGATE => {
                let Some(url) = text("url") else {
                    return self.agent_reply(conn, id, err("url is required"));
                };
                let r = if tool == tools::OPEN_TAB {
                    self.app.agent_open(ws, &url)
                } else {
                    self.app.agent_navigate(ws, tab, &url).map(|fx| (tab, fx))
                };
                match r {
                    Ok((t, fx)) => {
                        self.perform(fx);
                        Ok(Some(Wait { tab: t, ..wait(After::Report) }))
                    }
                    Err(AgentError::NeedsApproval { host }) => {
                        if let Some(h) = self.agents.as_mut() {
                            h.prompts.push_back(Prompt::Host { conn, id, session, host, tool: tool.into(), args });
                        }
                        return;
                    }
                    Err(e) => Err(e),
                }
            }
            tools::RELOAD => self.app.agent_reload(ws, tab).map(|fx| {
                self.perform(fx);
                Some(wait(After::Report))
            }),
            tools::CLOSE_TAB => self.app.agent_close(ws, tab).map(|fx| {
                self.perform(fx);
                None
            }),
            tools::SNAPSHOT | tools::SCREENSHOT => self.app.agent_wake(ws, tab).map(|fx| {
                let woke = !fx.is_empty();
                self.perform(fx);
                let then = if tool == tools::SNAPSHOT { After::Snapshot } else { After::Screenshot(text("element")) };
                let w = wait(then);
                if woke {
                    // Asleep a moment ago: look once it has loaded again.
                    Some(w)
                } else {
                    self.look(w);
                    None
                }
            }),
            tools::CONSOLE_LOGS | tools::NETWORK_REQUESTS => {
                let reply = self.captured(ws, tab, tool, &args);
                return self.agent_reply(conn, id, reply);
            }
            other => return self.agent_reply(conn, id, err(format!("unknown tool {other}"))),
        };

        match outcome {
            Err(e) => self.agent_reply(conn, id, err(e)),
            Ok(None) if tool == tools::CLOSE_TAB => self.agent_reply(conn, id, Reply::Ok(json!({"closed": tab_ref(tab)}))),
            Ok(None) => {}
            Ok(Some(mut w)) => {
                w.since = self.app.clock;
                if let Some(h) = self.agents.as_mut() {
                    h.waits.push(w);
                }
            }
        }
    }

    /// Console or network entries, filtered as asked.
    fn captured(&self, ws: usize, tab: TabId, tool: &str, args: &Value) -> Reply {
        if !self.app.workspaces.spaces[ws].tabs.iter().any(|t| t.id == tab) {
            return err(AgentError::NoSuchTab(tab));
        }
        let Some(c) = self.engine.capture(tab) else {
            return Reply::Ok(json!({
                "tab": tab_ref(tab),
                "entries": [],
                "note": "this tab is not loaded, so nothing is being captured; reload it to start again",
            }));
        };
        let since = args.get("since").and_then(Value::as_u64).unwrap_or(0);
        let entries: Vec<Value> = if tool == tools::CONSOLE_LOGS {
            let floor = match args.get("level").and_then(Value::as_str) {
                Some("error") => 4,
                Some("warn") => 3,
                Some("info") => 2,
                Some("log") => 1,
                _ => 0,
            };
            let rank = |l: Level| match l {
                Level::Debug => 0,
                Level::Log => 1,
                Level::Info => 2,
                Level::Warn => 3,
                Level::Error => 4,
                Level::Navigation => 5,
            };
            c.console_since(since)
                .filter(|e| rank(e.level) >= floor)
                .map(|e| {
                    let mut v = json!({"seq": e.seq, "level": e.level.label(), "text": e.text});
                    if !e.source.is_empty() {
                        v["source"] = json!(e.source);
                    }
                    if !e.stack.is_empty() {
                        v["stack"] = json!(e.stack);
                    }
                    v
                })
                .collect()
        } else {
            let failed_only = args.get("failed_only").and_then(Value::as_bool).unwrap_or(false);
            c.network_since(since)
                .filter(|e| !failed_only || e.failed())
                .map(|e| {
                    json!({
                        "seq": e.seq, "kind": e.kind.label(), "method": e.method, "url": e.url,
                        "status": e.status, "ms": e.ms, "error": e.error,
                    })
                })
                .collect()
        };
        Reply::Ok(json!({"tab": tab_ref(tab), "last_seq": c.last_seq(), "entries": entries}))
    }

    /// Answer every wait whose tab has finished loading, or run out of time.
    fn finish_waits(&mut self) {
        let Some(h) = self.agents.as_mut() else { return };
        if h.waits.is_empty() {
            return;
        }
        let now = self.app.clock;
        let waits = std::mem::take(&mut h.waits);
        let mut still = Vec::new();
        for w in waits {
            let loading = self.engine.load_state(w.tab).map(|(l, _)| l);
            let elapsed = now.saturating_sub(w.since);
            let done = elapsed >= LOAD_SETTLE_MS && loading != Some(true);
            if !done && elapsed < LOAD_TIMEOUT_MS {
                still.push(w);
                continue;
            }
            match w.then {
                After::Report => {
                    let reply = self.load_report(w.tab, done);
                    self.agent_reply(w.conn, w.id, reply);
                }
                _ => self.look(w),
            }
        }
        if let Some(h) = self.agents.as_mut() {
            h.waits.extend(still);
        }
    }

    /// Where a tab ended up after loading.
    fn load_report(&self, tab: TabId, finished: bool) -> Reply {
        let Some((_, t)) = self.app.workspaces.locate(tab) else {
            return err(format!("tab {} was closed while loading", tab_ref(tab)));
        };
        if t.kind == PageKind::Error {
            let why = t.error.map(|e| e.spec().name).unwrap_or("the page failed to load");
            return err(format!("{} could not load {}: {why}", tab_ref(tab), t.url));
        }
        let url = self.engine.url(tab).unwrap_or_else(|| t.url.clone());
        let title = self.engine.title(tab).unwrap_or_else(|| t.title.clone());
        let mut v = json!({"tab": tab_ref(tab), "url": url, "title": title});
        if !finished {
            v["note"] = json!("still loading after 20 s; what it shows may change");
        }
        Reply::Ok(v)
    }

    /// Snapshot or screenshot a live tab; the answer goes out from the
    /// engine's callback.
    fn look(&self, w: Wait) {
        let Some(h) = &self.agents else { return };
        let server = h.server.clone();
        let (conn, id, tab) = (w.conn, w.id, w.tab);
        match w.then {
            After::Snapshot => self.engine.snapshot(tab, move |r| {
                let reply = match r {
                    Ok(text) => Reply::Ok(json!({"tab": tab_ref(tab), "text": text})),
                    Err(e) => err(e),
                };
                server.reply(conn, id, &reply);
            }),
            After::Screenshot(target) => {
                let named = target.clone();
                self.engine.screenshot(tab, target.as_deref(), move |r| {
                let reply = match r {
                    Ok(png) => Reply::Ok(json!({
                        "tab": tab_ref(tab),
                        "element": named,
                        "png_base64": compact_agent::base64(&png),
                    })),
                    Err(e) => err(e),
                };
                server.reply(conn, id, &reply);
                })
            }
            After::Report => {}
        }
    }

    /// Put the next question on screen, if none is showing.
    fn show_next_prompt(&mut self) {
        let Some(h) = self.agents.as_mut() else { return };
        if h.showing {
            return;
        }
        let Some(prompt) = h.prompts.pop_front() else { return };
        h.showing = true;

        let (title, detail, allow) = match &prompt {
            Prompt::Hello { client, project, .. } => (
                format!("{client} wants a workspace"),
                format!(
                    "For {project}. It gets its own tabs and cookies, separate from yours, and can open \
                     pages on localhost and read what they show. It cannot see your tabs or logins, \
                     and asks before visiting any other site."
                ),
                "Allow",
            ),
            Prompt::Host { session, host, .. } => (
                format!("{} wants to open {host}", session.client),
                format!(
                    "In {}'s own workspace, with none of your logins. Allowing it lasts until Compact quits.",
                    session.client
                ),
                "Allow this session",
            ),
        };
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&title));
        alert.setInformativeText(&NSString::from_str(&detail));
        alert.addButtonWithTitle(&NSString::from_str(allow));
        alert.addButtonWithTitle(&NSString::from_str("Don't Allow"));

        let me = self.self_ref.clone();
        let prompt = std::cell::Cell::new(Some(prompt));
        let block = RcBlock::new(move |response: NSModalResponse| {
            let (Some(inner), Some(prompt)) = (me.upgrade(), prompt.take()) else { return };
            // First button: NSAlertFirstButtonReturn.
            let allowed = response == 1000;
            let mut i = inner.borrow_mut();
            i.answer_prompt(prompt, allowed);
            if let Some(h) = i.agents.as_mut() {
                h.showing = false;
            }
            i.show_next_prompt();
            i.render();
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
        // You may be in another app when an agent asks.
        NSApplication::sharedApplication(mtm).requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    }

    fn answer_prompt(&mut self, prompt: Prompt, allowed: bool) {
        match prompt {
            Prompt::Hello { conn, id, client, project } => {
                if allowed {
                    // Recorded even if the agent has hung up meanwhile: you
                    // said yes, so its retry should not ask again.
                    self.accept_agent(conn, id, &client, &project);
                } else {
                    self.agent_reply(conn, id, err(format!("the user did not allow {client} to use Compact")));
                }
            }
            Prompt::Host { conn, id, session, host, tool, args } => {
                if allowed {
                    let ws = self.agent_ws(&session);
                    self.app.allow_host(ws, &host);
                    self.agent_call(conn, id, session, &tool, args);
                } else {
                    self.agent_reply(conn, id, err(format!("the user did not allow opening {host}")));
                }
            }
        }
    }
}
