//! Markup for the internal pages.
//!
//! Pure string building over [`App`] — no engine, no ObjC, no I/O — so every
//! page can be rendered and asserted on in a unit test.
//!
//! Two deliberate departures from the canvas:
//!
//! - **No webfonts.** The canvas pulls Instrument Sans / IBM Plex / JetBrains
//!   Mono from Google Fonts. A browser that phones a third party every time you
//!   press ⌘T would contradict the entire product, so these pages use the
//!   system stack and `ui-monospace`.
//! - **The accent is the workspace's own colour** rather than a global
//!   preference. The new-tab page's whole job is telling you where you are, and
//!   the workspace dot is already the identity everything else uses.
//!
//! Clicks talk back over [`crate::bridge`]: every interactive element calls
//! `c('verb|arg')`, which the shell turns into an [`Action`](crate::command::Action).

use crate::page::{ErrorKind, PageKind};
use crate::settings::{NotifMode, Startup};
use crate::tab::TabMeta;
use crate::theme::{accent, bg, border, fg, fill, Rgba};
use crate::App;

/// Escape text for an HTML text node or a quoted attribute.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape text for a single-quoted JavaScript string literal.
fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' | '\r' => out.push(' '),
            '<' => out.push_str("\\x3c"),
            _ => out.push(c),
        }
    }
    out
}

/// First character of a label, for the square tint chips.
fn initial(s: &str) -> String {
    s.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Render whichever internal page this tab is showing.
///
/// `None` for a web page, which is the engine's business and not ours.
pub fn render(app: &App, tab: &TabMeta) -> Option<String> {
    match tab.kind {
        PageKind::Web => None,
        PageKind::NewTab => Some(new_tab(app, tab.scroll)),
        PageKind::Settings => Some(settings(app, tab.scroll)),
        PageKind::Error => Some(error(
            tab.error.unwrap_or(ErrorKind::NotFound),
            &tab.host,
            tab.scroll,
        )),
    }
}

/// Shared document shell: reset, tokens, animations, and the bridge.
fn shell(title: &str, accent: Rgba, scroll: u32, body: String) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  --accent: {accent};
  --page: {page};
  --card: {card};
  --sunk: {sunk};
  --line: {line};
  --hair: {hair};
  --fg: {fg};
  --fg2: {fg2};
  --fg3: {fg3};
  --fg4: {fg4};
  --hover: {hover};
  --mono: ui-monospace, "SF Mono", SFMono-Regular, Menlo, monospace;
}}
* {{ box-sizing: border-box; }}
html, body {{ margin:0; padding:0; height:100%; }}
body {{
  background: var(--page); color: var(--fg);
  font: 400 14px/1.5 -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  -webkit-font-smoothing: antialiased;
  /* No text cursor on what is chrome pretending to be a page. */
  -webkit-user-select: none; user-select: none;
}}
input {{ font-family: inherit; -webkit-user-select: text; user-select: text; }}
::selection {{ background: rgba(157,180,255,.28); }}
.mono {{ font-family: var(--mono); }}
.card {{ background: var(--card); border: 1px solid var(--line); border-radius: 13px; }}
.cap {{ font-family: var(--mono); font-size: 9.5px; letter-spacing: .11em; color: {cap}; text-transform: uppercase; }}
.click {{ cursor: default; }}
.row {{ display:flex; align-items:center; gap:10px; border-radius:8px; padding:0 9px; height:31px; transition: background .16s ease; }}
.row:hover {{ background: var(--hover); }}
.tint {{ flex:none; display:flex; align-items:center; justify-content:center; font-family: var(--mono); color:#0E0E12; }}
.ell {{ white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
@keyframes cpDrift {{ 0%,100% {{ transform:translateY(0) }} 50% {{ transform:translateY(-5px) }} }}
@keyframes cpBlink {{ 0%,45% {{ opacity:1 }} 50%,95% {{ opacity:.12 }} }}
@media (prefers-reduced-motion: reduce) {{ * {{ animation: none !important; transition: none !important; }} }}
</style></head><body>
{body}
<script>
// Every message carries the current scroll offset, because acting on one
// regenerates this document and the offset is the only thing that cannot be
// recovered from app state.
function c(m) {{
  try {{
    window.webkit.messageHandlers.compact.postMessage(Math.round(window.scrollY) + '\u001f' + m);
  }} catch (e) {{}}
}}
window.scrollTo(0, {scroll});
</script>
</body></html>"#,
        title = escape(title),
        accent = accent.css(),
        scroll = scroll,
        page = bg::PAGE.css(),
        card = Rgba::hex(0x131317).css(),
        sunk = bg::PAGE.css(),
        line = border::SUBTLE.css(),
        hair = Rgba::white(0.035).css(),
        fg = fg::PRIMARY.css(),
        fg2 = Rgba::hex(0xC6C6CE).css(),
        fg3 = Rgba::hex(0xA6A6B0).css(),
        fg4 = Rgba::hex(0x8A8A94).css(),
        hover = fill::HOVER_SOFT.css(),
        cap = Rgba::hex(0x7A7A86).css(),
    )
}

// ---- new tab ---------------------------------------------------------------

/// The page ⌘T lands on: search, what you were just reading, what you saved,
/// and where else you could be.
// Composed from independently-rendered sections. Clippy would rather see one
// flat `format!`; that would mean a single sixty-line template with every list
// interpolated inline, which is exactly what these helpers exist to avoid.
#[allow(clippy::format_in_format_args)]
pub fn new_tab(app: &App, scroll: u32) -> String {
    let ws = app.workspaces.current();
    let acc = ws.dot;
    let blocks = app.settings.blocks;

    let recents = if blocks.recents {
        let rows: String = app
            .history
            .iter()
            .take(5)
            .map(|h| {
                format!(
                    r#"<div class="row click" onclick="c('nav|{u}')">
<span class="tint" style="width:14px;height:14px;border-radius:4px;font-size:8px;background:{tint}">{ini}</span>
<span class="ell" style="font-size:12.5px;color:var(--fg3)">{title}</span>
<span class="mono" style="margin-left:auto;flex:none;font-size:9.5px;color:var(--fg4)">{host}</span>
</div>"#,
                    u = js(&h.url),
                    tint = h.tint().css(),
                    ini = escape(&initial(&h.title)),
                    title = escape(&h.title),
                    host = escape(&h.host),
                )
            })
            .collect();
        let body = if rows.is_empty() {
            format!(
                r#"<div style="font-size:12.5px;color:{c};padding:2px 4px">Nothing yet — everywhere you go shows up here.</div>"#,
                c = fg::DIM.css()
            )
        } else {
            rows
        };
        format!(
            r#"<div style="display:flex;flex-direction:column;gap:3px;min-height:0">
<div style="display:flex;align-items:center;gap:8px;padding:0 4px 8px">
<span class="cap">Recent</span><span class="mono" style="margin-left:auto;font-size:9.5px;color:{cap}">{n}</span></div>
{body}</div>"#,
            cap = Rgba::hex(0x7A7A86).css(),
            n = match app.history.len() {
                1 => "1 item".to_string(),
                n => format!("{n} items"),
            },
            body = body,
        )
    } else {
        String::new()
    };

    let pinned = if blocks.pinned {
        let cards: String = app
            .bookmarks
            .items()
            .iter()
            .take(6)
            .map(|b| {
                format!(
                    r#"<div class="click" onclick="c('bookmark|{id}')" style="min-width:0;border-radius:10px;padding:12px;display:flex;flex-direction:column;gap:10px;background:var(--sunk);border:1px solid {line};transition:transform .18s cubic-bezier(.16,1,.3,1),border-color .18s ease"
onmouseover="this.style.transform='translateY(-2px)';this.style.borderColor='{hi}'" onmouseout="this.style.transform='';this.style.borderColor='{line}'">
<span class="tint" style="width:20px;height:20px;border-radius:6px;font-size:9.5px;background:{tint}">{ini}</span>
<span class="ell" style="font-size:12px;color:var(--fg2)">{title}</span>
<span class="ell mono" style="font-size:10px;color:{host_fg}">{host}</span>
</div>"#,
                    id = b.id.0,
                    line = Rgba::white(0.05).css(),
                    hi = Rgba::white(0.14).css(),
                    tint = b.tint.css(),
                    ini = escape(&b.initial()),
                    title = escape(&b.title),
                    host_fg = Rgba::hex(0x8E8E98).css(),
                    host = escape(&b.host),
                )
            })
            .collect();
        format!(
            r#"<div class="card" style="min-width:0;padding:20px;display:flex;flex-direction:column;gap:14px">
<div style="display:flex;align-items:center;gap:8px"><span class="cap">Pinned</span>
<span class="mono" style="margin-left:auto;font-size:9.5px;color:{cap}">⌘D to add</span></div>
<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(132px,1fr));gap:9px">{cards}</div></div>"#,
            cap = Rgba::hex(0x7A7A86).css(),
            cards = cards,
        )
    } else {
        String::new()
    };

    let elsewhere = if blocks.workspaces {
        let rows: String = app
            .workspaces
            .spaces
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let here = i == app.workspaces.current;
                let badge = if w.unread > 0 {
                    format!(
                        r#"<span class="tint" style="min-width:18px;height:18px;padding:0 6px;border-radius:9px;font-size:9.5px;background:{dot}">{n}</span>"#,
                        dot = w.dot.css(),
                        n = w.unread
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<div class="row click" onclick="c('ws|{i}')" style="background:{bg}">
<span style="width:6px;height:6px;border-radius:50%;flex:none;background:{dot}"></span>
<span class="ell" style="font-size:12.5px;color:{fg}">{name}</span>
<span class="mono" style="margin-left:auto;flex:none;font-size:9.5px;color:var(--fg4)">{count}</span>
{badge}</div>"#,
                    i = i,
                    bg = if here { Rgba::white(0.05).css() } else { "transparent".into() },
                    dot = w.dot.css(),
                    fg = if here { fg::PRIMARY.css() } else { fg::SECONDARY.css() },
                    name = escape(&w.name),
                    count = escape(&w.count_label()),
                    badge = badge,
                )
            })
            .collect();
        format!(
            r#"<div class="card" style="min-width:0;padding:20px;display:flex;flex-direction:column;gap:12px">
<div style="display:flex;align-items:center;gap:8px"><span class="cap">Workspaces</span>
<span class="mono" style="margin-left:auto;font-size:9.5px;color:{cap}">⌘1–3</span></div>
<div style="display:flex;flex-direction:column;gap:3px">{rows}</div></div>"#,
            cap = Rgba::hex(0x7A7A86).css(),
            rows = rows,
        )
    } else {
        String::new()
    };

    // The right column only exists if something is in it; without this an
    // empty column leaves the search panel stranded at half width.
    let right = if pinned.is_empty() && elsewhere.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div style="min-width:0;display:flex;flex-direction:column;gap:14px">{pinned}{elsewhere}</div>"#
        )
    };
    let columns = if right.is_empty() {
        "minmax(0,1fr)"
    } else {
        "minmax(0,1.02fr) minmax(0,1.28fr)"
    };

    let body = format!(
        r#"<div style="min-height:100%;padding:74px 40px 34px;display:flex;justify-content:center">
<div style="width:100%;max-width:1060px;display:grid;grid-template-columns:{columns};gap:14px;align-content:start">

<div class="card" style="min-width:0;padding:22px;display:flex;flex-direction:column;gap:20px">
  <div style="display:flex;align-items:center;gap:9px">
    <span style="width:7px;height:7px;border-radius:50%;background:{dot}"></span>
    <span class="cap">{ws}</span>
    <span class="mono" style="margin-left:auto;font-size:9.5px;color:var(--fg4)">{open}</span>
  </div>

  <div style="display:flex;flex-direction:column;gap:11px">
    <div id="box" style="height:44px;border-radius:11px;display:flex;align-items:center;gap:11px;padding:0 14px;background:var(--sunk);border:1px solid {line};transition:border-color .2s ease">
      <span style="flex:none;font-size:11px;color:{dim}">⌁</span>
      <input id="q" autofocus spellcheck="false" placeholder="Search or enter address"
        onfocus="box.style.borderColor='var(--accent)';c('focus|1')" onblur="box.style.borderColor='{line}';c('focus|0')"
        onkeydown="if(event.key==='Enter'&&this.value.trim()){{c('nav|'+this.value.trim());this.value=''}}"
        style="flex:1;min-width:0;background:transparent;border:none;outline:none;font-family:var(--mono);font-size:13px;color:{input}">
      <span class="mono" style="flex:none;font-size:9.5px;color:{faint}">↵</span>
    </div>
    <div style="display:flex;align-items:center;gap:7px;font-size:11px;color:{dim2}">
      <span>Or press</span>
      <span class="mono click" onclick="c('palette')" style="font-size:9.5px;padding:3px 6px;border-radius:5px;color:{sec};background:{chipbg};border:1px solid {chipline}">⌘K</span>
      <span>for everything else</span>
    </div>
  </div>

  {recents}

  <div class="mono" style="margin-top:auto;display:flex;align-items:center;gap:10px;font-size:9.5px;color:var(--fg4)">
    <span>COMPACT</span>
    <span style="width:3px;height:3px;border-radius:50%;background:{sep}"></span>
    <span>{ram}</span>
  </div>
</div>

{right}
</div></div>"#,
        columns = columns,
        dot = acc.css(),
        ws = escape(&ws.name),
        open = format!("{} OPEN", ws.tabs.len()),
        line = border::SUBTLE.with_alpha(0.08).css(),
        dim = fg::DIM.css(),
        input = fg::INPUT.css(),
        faint = fg::FAINT.css(),
        dim2 = Rgba::hex(0x55555F).css(),
        sec = Rgba::hex(0x9A9AA4).css(),
        chipbg = Rgba::white(0.05).css(),
        chipline = Rgba::white(0.08).css(),
        sep = Rgba::hex(0x4A4A54).css(),
        ram = escape(&app.memory_label()),
        recents = recents,
        right = right,
    );

    shell("New tab", acc, scroll, body)
}

// ---- error -----------------------------------------------------------------

/// The page a failed navigation lands on.
pub fn error(kind: ErrorKind, host: &str, scroll: u32) -> String {
    let spec = kind.spec();
    let acc = accent::PRIMARY;

    let steps: String = spec
        .steps
        .iter()
        .map(|s| {
            let mark_fg = match s.ok {
                Some(true) => Rgba::hex(0x7FCF97),
                Some(false) => Rgba::hex(0xE58A8A),
                None => Rgba::hex(0x55555F),
            };
            format!(
                r#"<div style="display:flex;align-items:center;gap:11px;padding:10px 14px;border-bottom:1px solid var(--hair)">
<span class="mono" style="flex:none;width:14px;font-size:11px;color:{mark_fg}">{mark}</span>
<span style="font-size:12.5px;color:var(--fg3)">{label}</span>
<span class="mono" style="margin-left:auto;font-size:9.5px;color:var(--fg4)">{note}</span></div>"#,
                mark_fg = mark_fg.css(),
                mark = escape(s.mark),
                label = escape(s.label),
                note = escape(s.note),
            )
        })
        .collect();

    let body = format!(
        r#"<div style="min-height:100%;padding:96px 8% 60px;display:flex;align-items:flex-start;justify-content:center">
<div style="width:100%;max-width:640px;display:flex;flex-direction:column;gap:30px">

<div class="mono" style="display:flex;align-items:center;gap:10px;font-size:10.5px;letter-spacing:.1em;color:{crumb}">
  <span style="color:var(--accent)">{code}</span>
  <span style="width:3px;height:3px;border-radius:50%;background:{sep}"></span>
  <span>{host}</span>
</div>

<pre class="mono" style="margin:0;font-size:46px;line-height:1.06;letter-spacing:.14em;animation:cpDrift 5.5s ease-in-out infinite;color:var(--accent)">{glyph}</pre>

<div style="display:flex;flex-direction:column;gap:12px">
  <div style="font-size:29px;line-height:1.14;font-weight:600;letter-spacing:-.018em;color:{title_fg}">{title}</div>
  <div style="font-size:14.5px;line-height:1.55;color:var(--fg4);max-width:52ch">{msg}</div>
</div>

<div class="card" style="overflow:hidden;border-radius:11px">
  <div style="display:flex;align-items:center;gap:9px;padding:11px 14px;border-bottom:1px solid {line}">
    <span class="cap">What I checked</span>
    <span class="mono" style="margin-left:auto;font-size:9.5px;animation:cpBlink 1.4s steps(1,end) infinite;color:var(--accent)">●</span>
  </div>
  {steps}
</div>

<div style="display:flex;align-items:center;gap:9px;flex-wrap:wrap">
  <div class="click" onclick="c('retry')" style="height:33px;padding:0 14px;border-radius:8px;display:flex;align-items:center;gap:8px;font-size:12.5px;color:#0E0E12;background:var(--accent);transition:transform .16s ease"
    onmouseover="this.style.transform='translateY(-1px)'" onmouseout="this.style.transform=''">
    <span>{action}</span><span class="mono" style="font-size:9.5px;opacity:.7">⌘R</span>
  </div>
  <div class="click" onclick="c('home')" style="height:33px;padding:0 13px;border-radius:8px;display:flex;align-items:center;font-size:12.5px;color:{sec};border:1px solid {btnline}">New tab</div>
  <div class="click" onclick="c('address')" style="height:33px;padding:0 13px;border-radius:8px;display:flex;align-items:center;gap:8px;font-size:12.5px;color:{sec};border:1px solid {btnline}">
    <span>Edit address</span><span class="mono" style="font-size:9.5px;color:{dim2}">⌘L</span>
  </div>
</div>

<div class="mono" style="font-size:9.5px;line-height:1.7;color:{muted}">{sig}</div>
</div></div>"#,
        crumb = Rgba::hex(0x5B5B66).css(),
        code = escape(spec.code),
        sep = Rgba::hex(0x3A3A44).css(),
        host = escape(host),
        glyph = escape(spec.glyph),
        title_fg = fg::TITLE.css(),
        title = escape(spec.title),
        msg = escape(spec.body),
        line = Rgba::white(0.055).css(),
        steps = steps,
        action = escape(spec.action),
        sec = fg::SECONDARY.css(),
        btnline = Rgba::white(0.09).css(),
        dim2 = Rgba::hex(0x55555F).css(),
        muted = fg::MUTED.css(),
        sig = escape(spec.sig),
    );

    shell(spec.name, acc, scroll, body)
}

// ---- settings --------------------------------------------------------------

/// The full settings page (⌘⇧,).
pub fn settings(app: &App, scroll: u32) -> String {
    let s = &app.settings;
    let acc = accent::PRIMARY;

    let binds: String = s
        .keymap
        .rows()
        .map(|(b, keys)| {
            let rec = s.recording == Some(b);
            let editable = b.remappable();
            let (key_fg, key_bg, key_line) = if rec {
                ("#0E0E12".to_string(), acc.css(), acc.css())
            } else {
                (
                    Rgba::hex(0x787882).css(),
                    Rgba::white(0.05).css(),
                    Rgba::white(0.07).css(),
                )
            };
            format!(
                r#"<div{onclick} style="display:flex;align-items:center;gap:12px;padding:0 16px;height:42px;border-bottom:1px solid var(--hair);transition:background .16s ease{cursor}"
{hover}>
<span style="font-size:12.5px;color:{label_fg}">{label}</span>
<span class="mono" style="margin-left:auto;font-size:10.5px;padding:4px 8px;border-radius:6px;transition:all .16s ease;color:{key_fg};background:{key_bg};border:1px solid {key_line}">{keys}</span></div>"#,
                onclick = if editable {
                    format!(r#" class="click" onclick="c('record|{}')""#, b.slug())
                } else {
                    String::new()
                },
                cursor = if editable { "" } else { ";opacity:.62" },
                hover = if editable {
                    r#"onmouseover="this.style.background='rgba(255,255,255,.03)'" onmouseout="this.style.background=''""#
                } else {
                    ""
                },
                label_fg = Rgba::hex(0xB9B9C2).css(),
                label = escape(b.label()),
                keys = escape(if rec { "…" } else { &keys }),
            )
        })
        .collect();

    let notif_rules: String = app
        .workspaces
        .spaces
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let modes: String = NotifMode::ALL
                .into_iter()
                .map(|m| {
                    let on = s.notif_mode(i) == m;
                    format!(
                        r#"<div class="click" onclick="c('notif|{i}|{slug}')" style="height:25px;padding:0 11px;border-radius:6px;display:flex;align-items:center;font-size:11.5px;transition:all .16s ease;background:{bg};color:{fg}">{label}</div>"#,
                        i = i,
                        slug = m.slug(),
                        bg = if on { Rgba::white(0.1).css() } else { "transparent".into() },
                        fg = if on { fg::PRIMARY.css() } else { Rgba::hex(0x75757F).css() },
                        label = m.label(),
                    )
                })
                .collect();
            format!(
                r#"<div style="display:flex;align-items:center;gap:14px;padding:12px 16px;border-bottom:1px solid var(--hair)">
<span style="width:7px;height:7px;border-radius:50%;flex:none;background:{dot}"></span>
<span style="font-size:12.5px;color:var(--fg2)">{name}</span>
<div style="margin-left:auto;display:flex;gap:4px;padding:3px;border-radius:8px;background:{track};border:1px solid {line}">{modes}</div></div>"#,
                dot = w.dot.css(),
                name = escape(&w.name),
                track = Rgba::white(0.035).css(),
                line = Rgba::white(0.05).css(),
                modes = modes,
            )
        })
        .collect();

    let startup: String = Startup::ALL
        .into_iter()
        .map(|o| {
            let on = s.startup == o;
            format!(
                r#"<div class="row click" onclick="c('startup|{slug}')" style="height:34px;padding:0 10px;background:{bg}">
<span style="width:13px;height:13px;flex:none;border-radius:50%;display:flex;align-items:center;justify-content:center;border:1px solid {ring}">
<span style="width:6px;height:6px;border-radius:50%;background:{dot}"></span></span>
<span style="font-size:12.5px;color:{fg}">{label}</span></div>"#,
                slug = o.slug(),
                bg = if on { Rgba::white(0.05).css() } else { "transparent".into() },
                ring = if on { acc.css() } else { Rgba::white(0.16).css() },
                dot = if on { acc.css() } else { "transparent".into() },
                fg = if on { fg::PRIMARY.css() } else { fg::SECONDARY.css() },
                label = o.label(),
            )
        })
        .collect();

    let block_rows: String = [
        ("recents", "Recent", s.blocks.recents),
        ("pinned", "Pinned", s.blocks.pinned),
        ("workspaces", "Workspaces", s.blocks.workspaces),
    ]
    .into_iter()
    .map(|(slug, label, on)| {
        format!(
            r#"<div class="click" onclick="c('block|{slug}')" style="height:38px;display:flex;align-items:center;gap:12px;border-bottom:1px solid {line}">
<span style="font-size:12.5px;color:var(--fg2)">{label}</span>
<span style="margin-left:auto;flex:none;width:34px;height:19px;border-radius:10px;padding:2px;display:flex;transition:background .2s ease;background:{track}">
<span style="width:15px;height:15px;border-radius:50%;background:#fff;transition:transform .22s cubic-bezier(.16,1,.3,1);transform:translateX({knob})"></span>
</span></div>"#,
            line = Rgba::white(0.04).css(),
            track = if on { acc.css() } else { fill::TRACK_OFF.css() },
            knob = if on { "15px" } else { "0px" },
        )
    })
    .collect();

    let chips: String = ErrorKind::ALL
        .into_iter()
        .map(|k| {
            format!(
                r#"<div class="click" onclick="c('error|{slug}')" style="height:30px;padding:0 12px;border-radius:8px;display:flex;align-items:center;gap:8px;font-size:12px;color:{fg};transition:all .16s ease;border:1px solid {line}"
onmouseover="this.style.color='{hi}';this.style.background='{hibg}'" onmouseout="this.style.color='{fg}';this.style.background=''">
<span class="mono" style="font-size:9.5px;color:{dim}">{code}</span><span>{name}</span></div>"#,
                slug = k.slug(),
                fg = fg::SECONDARY.css(),
                line = Rgba::white(0.08).css(),
                hi = fg::PRIMARY.css(),
                hibg = Rgba::white(0.055).css(),
                dim = Rgba::hex(0x55555F).css(),
                code = escape(k.spec().code),
                name = escape(k.spec().name),
            )
        })
        .collect();

    let hint = if s.recording.is_some() {
        "press any combination — ESC to cancel"
    } else {
        "click a binding to remap"
    };

    let body = format!(
        r#"<div style="min-height:100%;padding:80px 7% 60px;display:flex;justify-content:center">
<div style="width:100%;max-width:860px;display:flex;flex-direction:column;gap:30px">

<div style="display:flex;align-items:baseline;gap:12px">
  <span style="font-size:30px;font-weight:600;letter-spacing:-.02em">Settings</span>
  <span class="mono" style="font-size:10px;letter-spacing:.1em;color:{faint}">COMPACT {ver}</span>
</div>

<div class="card" style="overflow:hidden;border-radius:12px">
  <div style="display:flex;align-items:center;gap:9px;padding:13px 16px;border-bottom:1px solid {line}">
    <span class="cap">Keybindings</span>
    <span style="margin-left:auto;font-size:11px;color:{dim2}">{hint}</span>
  </div>
  <div style="display:grid;grid-template-columns:1fr 1fr">{binds}</div>
</div>

<div class="card" style="overflow:hidden;border-radius:12px">
  <div style="display:flex;align-items:center;gap:9px;padding:13px 16px;border-bottom:1px solid {line}">
    <span class="cap">Notifications per workspace</span>
  </div>
  {notif_rules}
</div>

<div style="display:grid;grid-template-columns:1fr 1fr;gap:14px">
  <div class="card" style="padding:18px;display:flex;flex-direction:column;gap:14px;border-radius:12px">
    <span class="cap">On startup</span>
    <div style="display:flex;flex-direction:column;gap:3px">{startup}</div>
  </div>
  <div class="card" style="padding:18px;display:flex;flex-direction:column;gap:14px;border-radius:12px">
    <span class="cap">New tab blocks</span>
    <div style="display:flex;flex-direction:column;gap:2px">{block_rows}</div>
  </div>
</div>

<div class="card" style="padding:18px;display:flex;flex-direction:column;gap:16px;border-radius:12px">
  <span class="cap">Error page preview</span>
  <div style="display:flex;flex-wrap:wrap;gap:7px">{chips}</div>
</div>

</div></div>"#,
        faint = fg::FAINT.css(),
        ver = env!("CARGO_PKG_VERSION"),
        line = Rgba::white(0.055).css(),
        dim2 = Rgba::hex(0x55555F).css(),
        hint = hint,
        binds = binds,
        notif_rules = notif_rules,
        startup = startup,
        block_rows = block_rows,
        chips = chips,
    );

    shell("Settings", acc, scroll, body)
}
