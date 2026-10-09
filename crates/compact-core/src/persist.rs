//! What survives a restart, and the file format it survives in.
//!
//! The core stays free of I/O: this module turns an [`App`] into text and text
//! back into an [`App`], and the shell decides where that text lives and when it
//! is written. That keeps every rule about *what* is saved testable without a
//! file system.
//!
//! **Saved:** workspaces and their tabs (title, host, URL, page kind), which
//! agent owns an agent workspace, which agents you approved for which
//! projects, which tab and workspace were in front, bookmarks, the chrome configuration and the
//! settings page, keymap included.
//!
//! **Not saved, on purpose:** history (see [`crate::HISTORY_LIMIT`] — it is
//! meant to die with the session), notifications, unread counts, and anything
//! about live or hibernated web views. Every tab comes back `Cold` and loads by
//! URL when it is first needed.
//!
//! The format is line-based text rather than a serialization crate because the
//! core has no dependencies and this is a few dozen lines a user might
//! reasonably open in an editor. One record per line, fields separated by tabs,
//! with `\\`, `\t` and `\n` escaped. Unknown records are skipped so an older
//! build can read a newer file's known parts, but a newer *version* is refused
//! outright: the shell must not overwrite a file it only half understood.

use crate::bookmark::Bookmarks;
use crate::chrome::{ChromeConfig, TabLayout};
use crate::page::PageKind;
use crate::settings::{BindingId, Chord, NotifMode, Settings, Startup};
use crate::theme::Rgba;
use crate::review::{Anchor, Comment, CommentId, CommentKind, CommentState, Remark};
use crate::workspace::{AgentInfo, Control, WorkspaceSet};
use crate::App;

/// Bumped when a change would make an older build misread the file.
///
/// 2 added agent workspaces. A version-1 build would have skipped the `agent`
/// record and read the workspace as one of yours, then saved it back that way.
pub const FORMAT_VERSION: u32 = 2;

const MAGIC: &str = "compact-state";

/// Why a saved file could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// Not a Compact state file at all.
    NotState,
    /// Written by a newer Compact. The caller must not overwrite it.
    TooNew(u32),
    /// A record that should be readable was not.
    Corrupt { line: usize, reason: String },
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoadError::NotState => write!(f, "not a Compact state file"),
            LoadError::TooNew(v) => write!(
                f,
                "written by a newer Compact (format {v}, this build reads {FORMAT_VERSION})"
            ),
            LoadError::Corrupt { line, reason } => write!(f, "line {line}: {reason}"),
        }
    }
}

/// One tab as it is written to disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedTab {
    pub kind: PageKind,
    pub title: String,
    pub host: String,
    pub url: String,
}

/// One workspace as it is written to disk.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedSpace {
    pub name: String,
    /// Index into `tabs` of the tab that was in front.
    pub active: usize,
    pub tabs: Vec<SavedTab>,
    /// Present for an agent workspace.
    pub agent: Option<AgentInfo>,
}

// ---- encoding ----------------------------------------------------------------

/// Render `app`'s durable state as the text of a state file.
pub fn encode(app: &App) -> String {
    let mut out = String::new();
    line(&mut out, &[MAGIC, &FORMAT_VERSION.to_string()]);

    let c = &app.chrome;
    line(
        &mut out,
        &[
            "chrome",
            layout_slug(c.layout),
            &c.threshold.to_string(),
            flag(c.pinned),
            flag(c.smooth_motion),
            flag(c.dim_page),
            flag(c.show_hint),
        ],
    );

    let s = &app.settings;
    line(&mut out, &["startup", s.startup.slug()]);
    line(
        &mut out,
        &["blocks", flag(s.blocks.recents), flag(s.blocks.pinned), flag(s.blocks.workspaces)],
    );
    for b in BindingId::ALL.into_iter().filter(|b| b.remappable()) {
        let ch = s.keymap.chord(b);
        line(&mut out, &["bind", b.slug(), &mods_field(ch), &ch.key]);
    }

    let ws = &app.workspaces;
    line(&mut out, &["current", &ws.current.to_string()]);
    for (i, w) in ws.spaces.iter().enumerate() {
        line(
            &mut out,
            &["workspace", &w.name, s.notif_mode(i).slug(), &w.active_index().to_string()],
        );
        if let Some(a) = &w.agent {
            line(&mut out, &["agent", &a.client, &a.project]);
            for c in &a.comments {
                let n = &c.anchor;
                line(
                    &mut out,
                    &[
                        "comment",
                        &c.id.0.to_string(),
                        c.kind.slug(),
                        c.state.slug(),
                        flag(c.delivered),
                        &c.body,
                        &n.url,
                        &n.selector,
                        &n.text,
                        &n.role,
                        &n.data,
                        &floats(&n.rect),
                        &floats(&n.viewport),
                        &floats(&n.scroll),
                        &n.styles,
                        n.crop.as_deref().unwrap_or(""),
                        &n.framework,
                        &n.component,
                        &n.source,
                    ],
                );
                for r in &c.replies {
                    line(&mut out, &["remark", if r.from_agent { "agent" } else { "you" }, &r.body]);
                }
            }
        }
        for t in &w.tabs {
            let (kind, title, host, url) = saved_tab_fields(t);
            line(&mut out, &["tab", kind, title, host, url]);
        }
    }

    for b in app.bookmarks.items() {
        line(&mut out, &["bookmark", &b.title, &b.host, &rgba_field(b.tint)]);
    }
    for a in &app.approvals {
        line(&mut out, &["approved", &a.client, &a.project]);
    }
    out
}

/// What a tab is written as.
///
/// An error page is saved as the site it failed to reach, so a restart retries
/// it instead of reopening on an apology for a network that may be back.
fn saved_tab_fields(t: &crate::tab::TabMeta) -> (&'static str, &str, &str, &str) {
    let kind = match t.kind {
        PageKind::Web | PageKind::Error => "web",
        PageKind::NewTab => "newtab",
        PageKind::Settings => "settings",
    };
    (kind, &t.title, &t.host, &t.url)
}

fn line(out: &mut String, fields: &[&str]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        escape_into(out, f);
    }
    out.push('\n');
}

fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

const fn flag(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

const fn layout_slug(l: TabLayout) -> &'static str {
    match l {
        TabLayout::Vertical => "vertical",
        TabLayout::Horizontal => "horizontal",
    }
}

/// Modifiers as a fixed three-letter field: `c` `s` `a` when held, `-` when not.
fn mods_field(c: &Chord) -> String {
    [(c.cmd, 'c'), (c.shift, 's'), (c.alt, 'a')]
        .into_iter()
        .map(|(on, ch)| if on { ch } else { '-' })
        .collect()
}

fn floats(v: &[f64]) -> String {
    v.iter().map(f64::to_string).collect::<Vec<_>>().join(",")
}

fn parse_floats<const N: usize>(s: &str) -> Option<[f64; N]> {
    let v: Vec<f64> = s.split(',').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    v.try_into().ok()
}

/// `f64`'s `Display` round-trips exactly, so the tint comes back bit-for-bit.
fn rgba_field(c: Rgba) -> String {
    format!("{},{},{},{}", c.r, c.g, c.b, c.a)
}

// ---- decoding ----------------------------------------------------------------

/// Rebuild an [`App`] from a state file, applying the startup preference.
pub fn decode(text: &str) -> Result<App, LoadError> {
    let mut lines = text.lines().enumerate();

    let (_, header) = lines.next().ok_or(LoadError::NotState)?;
    let mut head = header.split('\t');
    if head.next() != Some(MAGIC) {
        return Err(LoadError::NotState);
    }
    let version: u32 = head.next().and_then(|v| v.parse().ok()).ok_or(LoadError::NotState)?;
    if version > FORMAT_VERSION {
        return Err(LoadError::TooNew(version));
    }

    let mut chrome = ChromeConfig::default();
    let mut settings = Settings::default();
    let mut current = 0usize;
    let mut spaces: Vec<SavedSpace> = Vec::new();
    let mut modes: Vec<NotifMode> = Vec::new();
    let mut bookmarks = Bookmarks::default();
    let mut approvals: Vec<crate::agent::Approval> = Vec::new();
    let mut comment_seq = 0u64;
    let mut binds: Vec<(BindingId, Chord)> = Vec::new();

    for (i, raw) in lines {
        let n = i + 1;
        if raw.is_empty() {
            continue;
        }
        let f: Vec<String> = raw.split('\t').map(unescape).collect();
        let bad = |reason: &str| LoadError::Corrupt { line: n, reason: reason.to_string() };
        let need = |count: usize| {
            if f.len() < count {
                Err(bad(&format!("expected {count} fields, found {}", f.len())))
            } else {
                Ok(())
            }
        };
        match f[0].as_str() {
            "chrome" => {
                need(7)?;
                chrome.layout = match f[1].as_str() {
                    "horizontal" => TabLayout::Horizontal,
                    _ => TabLayout::Vertical,
                };
                if let Ok(t) = f[2].parse::<f64>() {
                    chrome.set_threshold(t);
                }
                chrome.pinned = f[3] == "1";
                chrome.smooth_motion = f[4] == "1";
                chrome.dim_page = f[5] == "1";
                chrome.show_hint = f[6] == "1";
            }
            "startup" => {
                need(2)?;
                if let Some(s) = Startup::from_slug(&f[1]) {
                    settings.startup = s;
                }
            }
            "blocks" => {
                need(4)?;
                settings.blocks.recents = f[1] == "1";
                settings.blocks.pinned = f[2] == "1";
                settings.blocks.workspaces = f[3] == "1";
            }
            "bind" => {
                need(4)?;
                // A binding this build no longer has is dropped, not an error.
                if let Some(id) = BindingId::from_slug(&f[1]) {
                    let m: Vec<char> = f[2].chars().collect();
                    let held = |i: usize| m.get(i).is_some_and(|c| *c != '-');
                    binds.push((id, Chord::new(held(0), held(1), held(2), &f[3])));
                }
            }
            "current" => {
                need(2)?;
                current = f[1].parse().map_err(|_| bad("current is not a number"))?;
            }
            "workspace" => {
                need(4)?;
                modes.push(NotifMode::from_slug(&f[2]).unwrap_or(NotifMode::All));
                spaces.push(SavedSpace {
                    name: f[1].clone(),
                    active: f[3].parse().map_err(|_| bad("active tab is not a number"))?,
                    tabs: Vec::new(),
                    agent: None,
                });
            }
            "agent" => {
                need(3)?;
                let space = spaces.last_mut().ok_or_else(|| bad("agent before any workspace"))?;
                // Control is not saved: nobody is driving a workspace whose
                // agent has not reconnected yet, and the agent asks again.
                space.agent = Some(AgentInfo {
                    client: f[1].clone(),
                    project: f[2].clone(),
                    control: Control::Agent,
                    hosts: Vec::new(),
                    connected: false,
                    comments: Vec::new(),
                });
            }
            "tab" => {
                need(5)?;
                let space = spaces.last_mut().ok_or_else(|| bad("tab before any workspace"))?;
                let kind = match f[1].as_str() {
                    "newtab" => PageKind::NewTab,
                    "settings" => PageKind::Settings,
                    _ => PageKind::Web,
                };
                space.tabs.push(SavedTab {
                    kind,
                    title: f[2].clone(),
                    host: f[3].clone(),
                    url: f[4].clone(),
                });
            }
            "bookmark" => {
                need(4)?;
                let tint = parse_rgba(&f[3]).ok_or_else(|| bad("unreadable bookmark tint"))?;
                bookmarks.insert(f[1].clone(), f[2].clone(), tint);
            }
            "comment" => {
                need(16)?;
                let agent = spaces
                    .last_mut()
                    .and_then(|s| s.agent.as_mut())
                    .ok_or_else(|| bad("comment outside an agent workspace"))?;
                let id: u64 = f[1].parse().map_err(|_| bad("comment id is not a number"))?;
                comment_seq = comment_seq.max(id);
                agent.comments.push(Comment {
                    id: CommentId(id),
                    kind: CommentKind::from_slug(&f[2]).unwrap_or(CommentKind::Change),
                    state: CommentState::from_slug(&f[3]).unwrap_or(CommentState::Open),
                    delivered: f[4] == "1",
                    body: f[5].clone(),
                    anchor: Anchor {
                        url: f[6].clone(),
                        selector: f[7].clone(),
                        text: f[8].clone(),
                        role: f[9].clone(),
                        data: f[10].clone(),
                        rect: parse_floats(&f[11]).unwrap_or_default(),
                        viewport: parse_floats(&f[12]).unwrap_or_default(),
                        scroll: parse_floats(&f[13]).unwrap_or_default(),
                        styles: f[14].clone(),
                        crop: Some(f[15].clone()).filter(|c| !c.is_empty()),
                        // Added after the record itself; files from before
                        // simply lack them.
                        framework: f.get(16).cloned().unwrap_or_default(),
                        component: f.get(17).cloned().unwrap_or_default(),
                        source: f.get(18).cloned().unwrap_or_default(),
                    },
                    replies: Vec::new(),
                });
            }
            "remark" => {
                need(3)?;
                let c = spaces
                    .last_mut()
                    .and_then(|s| s.agent.as_mut())
                    .and_then(|a| a.comments.last_mut())
                    .ok_or_else(|| bad("remark before any comment"))?;
                c.replies.push(Remark { from_agent: f[1] == "agent", body: f[2].clone() });
            }
            "approved" => {
                need(3)?;
                approvals.push(crate::agent::Approval { client: f[1].clone(), project: f[2].clone() });
            }
            // Written by a newer build of the same format version.
            _ => {}
        }
    }

    // A workspace saved with no tabs would break the never-empty invariant the
    // rest of the core relies on, so it reopens on a new tab instead.
    for s in &mut spaces {
        if s.tabs.is_empty() {
            s.tabs.push(new_tab_entry());
            s.active = 0;
        }
    }
    if spaces.is_empty() {
        return Ok(App::fresh());
    }

    for (id, chord) in binds {
        settings.keymap.rebind(id, chord);
    }
    settings.notif_modes = modes;

    let mut app = App::fresh();
    app.chrome = chrome;
    app.settings = settings;
    app.bookmarks = bookmarks;
    app.approvals = approvals;
    app.comment_seq = comment_seq;
    app.workspaces = WorkspaceSet::from_saved(&spaces, current);
    apply_startup(&mut app);
    Ok(app)
}

fn parse_rgba(s: &str) -> Option<Rgba> {
    let v: Vec<f64> = s.split(',').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    match v[..] {
        [r, g, b, a] => Some(Rgba::new(r, g, b, a)),
        _ => None,
    }
}

pub(crate) fn new_tab_entry() -> SavedTab {
    SavedTab {
        kind: PageKind::NewTab,
        title: "New tab".into(),
        host: crate::page::NEW_TAB_URL.into(),
        url: crate::page::NEW_TAB_URL.into(),
    }
}

/// Honour the "On startup" setting.
///
/// Tabs are always restored, whichever option is chosen. The setting decides
/// what you *land on*, not whether your workspaces survive: a browser built
/// around workspaces that emptied them on every launch would be losing the one
/// thing it promises to keep.
///
/// - **Open a new tab:** a fresh new-tab page in front of the last workspace.
/// - **Restore last session:** exactly where you left off.
/// - **Jump to pinned workspace:** the first workspace. There is no separate
///   notion of a pinned workspace yet, and the first is the one `⌘1` reaches.
fn apply_startup(app: &mut App) {
    match app.settings.startup {
        Startup::NewTab => {
            // The new tab is for you, so it never lands in an agent's workspace.
            if app.workspaces.current().is_agent() {
                app.workspaces.current = 0;
            }
            app.workspaces.new_tab();
        }
        Startup::RestoreSession => {}
        Startup::PinnedWorkspace => {
            app.workspaces.current = 0;
        }
    }
}
