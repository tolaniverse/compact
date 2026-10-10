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
    /// Agents blocked in `wait_for_feedback`.
    feedback: Vec<FeedbackWait>,
    /// Comments whose crop is still being taken. They are held back from
    /// agents until it is saved, so a comment never arrives without the
    /// picture of what it is about.
    cropping: std::collections::HashSet<compact_core::review::CommentId>,
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
            feedback: Vec::new(),
            cropping: Default::default(),
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
        let had_work = !events.is_empty() || !host.waits.is_empty() || !host.feedback.is_empty();
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
                    h.feedback.retain(|w| w.conn != conn);
                    // A queued question for an agent that left is moot; the
                    // one on screen stays, since your answer still counts.
                    h.prompts.retain(|p| match p {
                        Prompt::Hello { conn: c, .. } | Prompt::Host { conn: c, .. } => *c != conn,
                    });
                }
            }
        }
        self.finish_waits();
        self.finish_feedback();
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
        if self.review_call((conn, id), &session, ws, tab, tool, &args)
            || self.devtools_call((conn, id), ws, tab, tool, &args)
        {
            return;
        }
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

// ---- review mode ---------------------------------------------------------------

use compact_core::review::{Anchor, Comment, CommentKind, CommentState};

/// Space kept around an element in its crop, in CSS pixels.
const CROP_MARGIN: f64 = 24.0;

/// Where comment crops are kept: `crops/` beside the state file.
fn crops_dir() -> Option<std::path::PathBuf> {
    compact_agent::paths::state_dir().map(|d| d.join("crops"))
}

/// A crop's file, if `name` is one Compact would have written. The name comes
/// from the state file, so it is checked rather than joined blindly.
fn crop_path(name: &str) -> Option<std::path::PathBuf> {
    let ok = name.strip_prefix('c').and_then(|r| r.strip_suffix(".png")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| crops_dir().map(|d| d.join(name))).flatten()
}

/// Switch the overlay on `tab` on or off, with its current pins.
pub(super) fn show_review(app: &compact_core::App, engine: &compact_engine::WebKitEngine, tab: TabId, on: bool) {
    let agent = app
        .workspaces
        .locate(tab)
        .and_then(|(ws, _)| app.workspaces.spaces[ws].agent.as_ref())
        .map(|a| a.client.clone())
        .unwrap_or_else(|| "the agent".into());
    engine.set_review(tab, on, &pins_json(app, tab), &agent);
}

/// Bring the overlay under review up to date after its comments changed.
fn refresh_review(app: &compact_core::App, engine: &compact_engine::WebKitEngine) {
    if let Some(tab) = app.review {
        show_review(app, engine, tab, true);
    }
}

/// The pins the overlay draws on `tab`, as the JSON its `set` takes.
pub(super) fn pins_json(app: &compact_core::App, tab: TabId) -> String {
    let pins: Vec<Value> = app
        .pins_for(tab)
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let a = &c.anchor;
            json!({
                "id": c.id.label(), "n": i + 1, "state": c.state.slug(), "kind": c.kind.slug(), "body": c.body,
                "selector": a.selector, "text": a.text, "role": a.role,
                "rect": a.rect, "viewport": a.viewport, "scroll": a.scroll,
                "replies": c.replies.iter().map(|r| json!({"from": if r.from_agent { "agent" } else { "you" }, "body": r.body})).collect::<Vec<_>>(),
            })
        })
        .collect();
    Value::Array(pins).to_string()
}

/// A comment as an agent reads it. `image` is its crop's position among the
/// images sent with it, when it has one.
fn comment_json(c: &Comment, image: Option<usize>) -> Value {
    let a = &c.anchor;
    let element = (!a.selector.is_empty()).then(|| {
        let mut e = json!({"selector": a.selector, "text": a.text, "role": a.role, "data": a.data, "styles": a.styles});
        if !a.component.is_empty() || !a.source.is_empty() {
            e["component"] = json!(a.component);
            e["source"] = json!(a.source);
            e["source_reported_by"] = json!(format!("the page's {} dev build", a.framework));
        }
        e
    });
    json!({
        "id": c.id.label(),
        "kind": c.kind.slug(),
        "state": c.state.slug(),
        "body": c.body,
        "page": a.url,
        "element": element,
        "area": {"x": a.rect[0], "y": a.rect[1], "width": a.rect[2], "height": a.rect[3]},
        "viewport": {"width": a.viewport[0], "height": a.viewport[1]},
        "replies": c.replies.iter().map(|r| json!({"from": if r.from_agent { "agent" } else { "user" }, "body": r.body})).collect::<Vec<_>>(),
        "screenshot": image.map(|i| format!("image {} above", i + 1)),
    })
}

/// Comments with their crops attached, ready to send.
fn comments_reply(comments: &[Comment], note: Option<&str>) -> Reply {
    let mut images = Vec::new();
    let list: Vec<Value> = comments
        .iter()
        .map(|c| {
            let png = c.anchor.crop.as_deref().and_then(crop_path).and_then(|p| std::fs::read(p).ok());
            let image = png.map(|bytes| {
                images.push(json!({"png_base64": compact_agent::base64(&bytes)}));
                images.len() - 1
            });
            comment_json(c, image)
        })
        .collect();
    let mut v = json!({"comments": list, "images": images});
    if let Some(n) = note {
        v["note"] = json!(n);
    }
    Reply::Ok(v)
}

pub(super) struct FeedbackWait {
    conn: ConnId,
    id: u64,
    session: Session,
    until: u64,
}

impl Inner {
    /// A message from the review overlay on one of the agent's pages.
    pub(super) fn on_review_message(&mut self, view: usize, body: &str) {
        let Some(tab) = self.engine.tab_of(view) else { return };
        let Ok(v) = serde_json::from_str::<Value>(body) else { return };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let floats = |k: &str| -> Vec<f64> {
            v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
        };
        match s("type").as_str() {
            "ready" => {
                if self.app.review == Some(tab) {
                    show_review(&self.app, &self.engine, tab, true);
                }
            }
            "resolve" | "reply" => {
                // Only a comment on this tab's own workspace, whatever id the
                // message names.
                let Some(id) = compact_core::review::CommentId::parse(&s("id")) else { return };
                let Some((ws, _)) = self.app.workspaces.locate(tab) else { return };
                if !self.app.comments_in(ws, None).iter().any(|c| c.id == id) {
                    return;
                }
                let changed = if s("type") == "resolve" {
                    self.app.resolve_comment(id)
                } else {
                    self.app.reply_comment(id, &s("body"))
                };
                if changed {
                    show_review(&self.app, &self.engine, tab, self.app.review == Some(tab));
                    self.render();
                }
            }
            "exit" => {
                let fx = self.app.end_review(tab);
                self.settle(fx);
            }
            "comment" => {
                let rect: [f64; 4] = floats("rect").try_into().unwrap_or_default();
                let anchor = Anchor {
                    url: s("url"),
                    selector: s("selector"),
                    text: s("text"),
                    role: s("role"),
                    data: s("data"),
                    rect,
                    viewport: floats("viewport").try_into().unwrap_or_default(),
                    scroll: floats("scroll").try_into().unwrap_or_default(),
                    styles: s("styles"),
                    crop: None,
                    framework: s("framework"),
                    component: s("component"),
                    source: s("source"),
                };
                let kind = CommentKind::from_slug(&s("kind")).unwrap_or(CommentKind::Change);
                let Some(id) = self.app.add_comment(tab, kind, &s("body"), anchor) else {
                    // Not one we can keep; show the overlay again as it was.
                    show_review(&self.app, &self.engine, tab, self.app.review == Some(tab));
                    return;
                };
                self.render();
                if let Some(h) = self.agents.as_mut() {
                    h.cropping.insert(id);
                }
                let [x, y, w, h] = rect;
                let area = (x - CROP_MARGIN, y - CROP_MARGIN, w + 2.0 * CROP_MARGIN, h + 2.0 * CROP_MARGIN);
                let me = self.self_ref.clone();
                self.engine.screenshot_rect(tab, area, move |shot| {
                    // Never on the caller's stack: a tab that is not live
                    // answers at once, while the shell is still borrowed.
                    crate::widgets::queue_after(0.0, move || {
                        let Some(inner) = me.upgrade() else { return };
                        let mut i = inner.borrow_mut();
                        if let Some(h) = i.agents.as_mut() {
                            h.cropping.remove(&id);
                        }
                        if let (Ok(png), Some(dir)) = (shot, crops_dir()) {
                            let name = format!("{}.png", id.label());
                            if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(&name), png)).is_ok() {
                                i.app.set_comment_crop(id, &name);
                            }
                        }
                        let on = i.app.review == Some(tab);
                        show_review(&i.app, &i.engine, tab, on);
                        i.render();
                    });
                });
            }
            _ => {}
        }
    }

    /// The review tools. Returns false for a tool that is not one of them.
    fn review_call(&mut self, (conn, id): (ConnId, u64), session: &Session, ws: usize, tab: TabId, tool: &str, args: &Value) -> bool {
        match tool {
            tools::REQUEST_REVIEW => {
                let fx = match self.app.agent_wake(ws, tab) {
                    Ok(fx) => fx,
                    Err(e) => {
                        self.agent_reply(conn, id, err(e));
                        return true;
                    }
                };
                self.perform(fx);
                let note = args.get("note").and_then(Value::as_str);
                let fx = self.app.request_review(ws, tab, note);
                self.settle(fx);
                self.agent_reply(conn, id, Reply::Ok(json!({
                    "tab": tab_ref(tab),
                    "note": "The user was told; call wait_for_feedback for their comments.",
                })));
            }
            tools::GET_COMMENTS => {
                let state = match args.get("state").and_then(Value::as_str) {
                    Some("all") => None,
                    Some(s) => CommentState::from_slug(s).or(Some(CommentState::Open)),
                    None => Some(CommentState::Open),
                };
                let comments: Vec<Comment> = self.app.comments_in(ws, state).into_iter().cloned().collect();
                if state == Some(CommentState::Open) || state.is_none() {
                    self.app.mark_delivered(ws);
                }
                self.agent_reply(conn, id, comments_reply(&comments, None));
            }
            tools::WAIT_FOR_FEEDBACK => {
                let secs = args
                    .get("timeout_seconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(120)
                    .clamp(1, tools::MAX_FEEDBACK_WAIT_SECS);
                let cropping = self.agents.as_ref().is_some_and(|h| !h.cropping.is_empty());
                if self.app.has_undelivered(ws) && !cropping {
                    let got = self.app.take_undelivered(ws);
                    self.agent_reply(conn, id, comments_reply(&got, None));
                } else if let Some(h) = self.agents.as_mut() {
                    h.feedback.push(FeedbackWait { conn, id, session: session.clone(), until: self.app.clock + secs * 1000 });
                }
            }
            tools::REPLY_TO_COMMENT | tools::MARK_ADDRESSED => {
                let text = |k: &str| args.get(k).and_then(Value::as_str);
                let Some(cid) = text("comment").and_then(compact_core::review::CommentId::parse) else {
                    self.agent_reply(conn, id, err("comment is required, like \"c3\""));
                    return true;
                };
                let result = if tool == tools::REPLY_TO_COMMENT {
                    self.app.agent_reply_comment(ws, cid, text("body").unwrap_or("")).map(|()| Vec::new())
                } else {
                    self.app.mark_addressed(ws, cid, text("note"))
                };
                match result {
                    Ok(fx) => {
                        let reloaded = !fx.is_empty();
                        // A reloaded page asks for its pins itself once it is
                        // back; one that was not needs them pushed.
                        self.settle(fx);
                        if !reloaded {
                            refresh_review(&self.app, &self.engine);
                        }
                        let state = self.app.comments_in(ws, None).iter().find(|c| c.id == cid).map(|c| c.state.slug());
                        self.agent_reply(conn, id, Reply::Ok(json!({"comment": cid.label(), "state": state, "reloaded": reloaded})));
                    }
                    Err(e) => self.agent_reply(conn, id, err(e)),
                }
            }
            _ => return false,
        }
        true
    }

    /// Answer feedback waits that now have comments, or have run out.
    fn finish_feedback(&mut self) {
        let Some(h) = self.agents.as_mut() else { return };
        if h.feedback.is_empty() {
            return;
        }
        let cropping = !h.cropping.is_empty();
        let waits = std::mem::take(&mut h.feedback);
        let mut still = Vec::new();
        for w in waits {
            let ws = self.app.agent_workspace(&w.session.client, &w.session.project);
            match ws {
                Some(ws) if self.app.has_undelivered(ws) && !cropping => {
                    let got = self.app.take_undelivered(ws);
                    self.agent_reply(w.conn, w.id, comments_reply(&got, None));
                }
                _ if self.app.clock >= w.until => {
                    self.agent_reply(w.conn, w.id, comments_reply(&[], Some("no new comments yet; call wait_for_feedback again to keep waiting")));
                }
                _ => still.push(w),
            }
        }
        if let Some(h) = self.agents.as_mut() {
            h.feedback.extend(still);
        }
    }
}

// ---- developer tools -------------------------------------------------------------

/// Captured entries after `since`, as the drawer's `add` takes them.
fn devtools_entries(c: &compact_engine::capture::Capture, since: u64) -> String {
    let mut all: Vec<(u64, Value)> = c
        .console_since(since)
        .map(|e| {
            (e.seq, json!({"type": "console", "seq": e.seq, "level": e.level.label(), "text": e.text, "source": e.source, "stack": e.stack}))
        })
        .chain(c.network_since(since).map(|e| {
            (e.seq, json!({"type": "net", "seq": e.seq, "kind": e.kind.label(), "method": e.method, "url": e.url,
                "status": e.status, "ms": e.ms, "error": e.error}))
        }))
        .collect();
    all.sort_by_key(|(s, _)| *s);
    Value::Array(all.into_iter().map(|(_, v)| v).collect()).to_string()
}

fn agent_name(app: &compact_core::App, tab: TabId) -> String {
    app.workspaces
        .locate(tab)
        .and_then(|(ws, _)| app.workspaces.spaces[ws].agent.as_ref())
        .map(|a| a.client.clone())
        .unwrap_or_else(|| "the agent".into())
}

impl Inner {
    /// Open or close the drawer on `tab`. It lives in a view of its own
    /// beside the page; opening it makes room for it.
    pub(super) fn show_devtools(&mut self, tab: TabId, on: bool, seq: Option<u64>) {
        let _ = tab;
        if on {
            self.tools_flash = seq;
            if self.tools_ready {
                self.tools_point();
            }
            self.layout();
        } else {
            self.engine.close_tools();
            self.engine.set_page_frame(None);
            self.tools_ready = false;
            self.layout();
        }
    }

    /// Tell the drawer which page it is looking at, and hand it everything
    /// captured there.
    fn tools_point(&mut self) {
        let Some(tab) = self.app.devtools else { return };
        let url = self.engine.url(tab).unwrap_or_default();
        let dock = self.app.settings.devtools_dock.slug();
        let seq = self.tools_flash.take();
        if let Some(c) = self.engine.capture(tab) {
            let json = devtools_entries(c, 0);
            self.devtools_sent = c.last_seq();
            self.engine.tools_eval(&compact_engine::devtools::add_script(&json, true));
        }
        self.engine.tools_eval(&compact_engine::devtools::open_script(seq, &agent_name(&self.app, tab), dock, &url));
    }

    /// Send the open drawer whatever was captured since last time.
    pub(super) fn feed_devtools(&mut self) {
        let Some(tab) = self.app.devtools else { return };
        if !self.tools_ready {
            return;
        }
        let Some(c) = self.engine.capture(tab) else { return };
        if c.last_seq() <= self.devtools_sent {
            return;
        }
        let json = devtools_entries(c, self.devtools_sent);
        self.devtools_sent = c.last_seq();
        self.engine.tools_eval(&compact_engine::devtools::add_script(&json, false));
    }

    /// A message from the drawer's own view.
    pub(super) fn on_tools_message(&mut self, body: &str) {
        let Ok(v) = serde_json::from_str::<Value>(body) else { return };
        let Some(tab) = self.app.devtools else { return };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "devtools-ready" => {
                self.tools_ready = true;
                self.tools_point();
            }
            "devtools-close" => {
                let fx = self.app.end_devtools(tab);
                self.settle(fx);
            }
            "devtools-dock" => {
                let d = v.get("dock").and_then(Value::as_str).and_then(compact_core::settings::Dock::from_slug);
                if let Some(d) = d {
                    self.app.settings.devtools_dock = d;
                    self.layout();
                }
            }
            "measure" => {
                let me = self.self_ref.clone();
                self.engine.performance(tab, move |r| {
                    let json = r.unwrap_or_else(|_| "null".into());
                    crate::widgets::queue_after(0.0, move || {
                        if let Some(inner) = me.upgrade() {
                            inner.borrow().engine.tools_eval(&compact_engine::devtools::perf_script(&json));
                        }
                    });
                });
            }
            // "Send to agent": a review comment on the inspected page.
            "comment" => {
                if let Some(view) = self.engine.view(tab).map(|v| v as *const _ as usize) {
                    self.on_review_message(view, body);
                }
            }
            _ => {}
        }
    }

    /// `performance` and `show_in_devtools`. Returns false for other tools.
    fn devtools_call(&mut self, (conn, id): (ConnId, u64), ws: usize, tab: TabId, tool: &str, args: &Value) -> bool {
        match tool {
            tools::PERFORMANCE => {
                let fx = match self.app.agent_wake(ws, tab) {
                    Ok(fx) => fx,
                    Err(e) => {
                        self.agent_reply(conn, id, err(e));
                        return true;
                    }
                };
                if !fx.is_empty() {
                    // Asleep a moment ago: a fresh load would measure a
                    // different page than the one the agent asked about.
                    self.perform(fx);
                    self.agent_reply(conn, id, err("the tab was not loaded; it is loading now, call performance again in a moment"));
                    return true;
                }
                let Some(server) = self.agents.as_ref().map(|h| h.server.clone()) else { return true };
                let footprint = self.app.workspaces.locate(tab).map(|(_, t)| t.effective_footprint() / (1024 * 1024));
                self.engine.performance(tab, move |r| {
                    let reply = match r.and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())) {
                        Ok(mut v) => {
                            v["tab"] = json!(tab_ref(tab));
                            v["compact_memory_estimate_mb"] = json!(footprint);
                            Reply::Ok(v)
                        }
                        Err(e) => err(e),
                    };
                    server.reply(conn, id, &reply);
                });
            }
            tools::SHOW_IN_DEVTOOLS => {
                let fx = match self.app.agent_wake(ws, tab) {
                    Ok(fx) => fx,
                    Err(e) => {
                        self.agent_reply(conn, id, err(e));
                        return true;
                    }
                };
                self.perform(fx);
                let seq = args.get("seq").and_then(Value::as_u64);
                let note = args.get("note").and_then(Value::as_str);
                let fx = self.app.show_in_devtools(ws, tab, seq, note);
                self.settle(fx);
                self.agent_reply(conn, id, Reply::Ok(json!({"tab": tab_ref(tab), "shown": true})));
            }
            _ => return false,
        }
        true
    }
}
