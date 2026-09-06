//! The internal pages' way of talking back.
//!
//! Pages emit `verb|arg|arg` strings; this module turns them into the same
//! [`Action`]s a keystroke or a click on the chrome produces. Nothing here can
//! reach the engine or the window — a page can only ask for something the
//! keyboard could already have asked for, which is the point.
//!
//! The wire format is pipe-delimited rather than JSON so the core stays
//! dependency-free. Only the *first* separator is significant for verbs that
//! carry free text, so a URL containing `|` survives the trip.

use crate::bookmark::BookmarkId;
use crate::command::Action;
use crate::page::ErrorKind;
use crate::settings::{BindingId, NotifMode, Startup};

/// A request from an internal page.
///
/// Most map straight onto an [`Action`]; the rest mutate settings, which is
/// state no keystroke reaches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PageMessage {
    /// Address-bar semantics: a URL or a search, from the new-tab field or a
    /// recent row.
    Navigate(String),
    Do(Action),
    OpenBookmark(BookmarkId),
    SwitchWorkspace(usize),
    SetStartup(Startup),
    ToggleBlock(String),
    SetNotifMode(usize, NotifMode),
    /// Begin (or cancel) recording a new chord for a binding.
    Record(BindingId),
    /// A text field inside an internal page took or lost the keyboard.
    FieldFocus(bool),
}

/// The separator between a message's scroll prefix and its body.
///
/// ASCII unit separator: it cannot occur in a URL, a slug or a number, so the
/// split is unambiguous without escaping.
const UNIT: char = '\u{1f}';

/// A parsed message, plus where the page was scrolled to when it was sent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Incoming {
    pub scroll: u32,
    pub message: PageMessage,
}

/// Parse one message. Unknown or malformed input is dropped rather than
/// guessed at: these strings come from a web view, and the only safe reading of
/// an unrecognised one is that it is not ours.
pub fn parse(raw: &str) -> Option<Incoming> {
    let (scroll, raw) = match raw.split_once(UNIT) {
        Some((n, rest)) => (n.parse().unwrap_or(0), rest),
        None => (0, raw),
    };
    parse_body(raw).map(|message| Incoming { scroll, message })
}

fn parse_body(raw: &str) -> Option<PageMessage> {
    let (verb, rest) = match raw.split_once('|') {
        Some((v, r)) => (v, r),
        None => (raw, ""),
    };
    match verb {
        "nav" => {
            let t = rest.trim();
            (!t.is_empty()).then(|| PageMessage::Navigate(t.to_string()))
        }
        "palette" => Some(PageMessage::Do(Action::OpenPalette)),
        "address" => Some(PageMessage::Do(Action::FocusAddress)),
        "retry" => Some(PageMessage::Do(Action::Retry)),
        "home" => Some(PageMessage::Do(Action::GoHome)),
        "newtab" => Some(PageMessage::Do(Action::NewTab)),
        "bookmark" => rest.parse().ok().map(|n| PageMessage::OpenBookmark(BookmarkId(n))),
        "ws" => rest.parse().ok().map(PageMessage::SwitchWorkspace),
        "error" => ErrorKind::from_slug(rest).map(|k| PageMessage::Do(Action::ShowError(k))),
        "startup" => Startup::from_slug(rest).map(PageMessage::SetStartup),
        "block" => Some(PageMessage::ToggleBlock(rest.to_string())),
        "notif" => {
            let (i, mode) = rest.split_once('|')?;
            Some(PageMessage::SetNotifMode(
                i.parse().ok()?,
                NotifMode::from_slug(mode)?,
            ))
        }
        "record" => BindingId::from_slug(rest).map(PageMessage::Record),
        "focus" => Some(PageMessage::FieldFocus(rest == "1")),
        _ => None,
    }
}
