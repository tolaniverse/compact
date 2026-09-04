//! AppKit chrome for Compact.
//!
//! The chrome is drawn with Core Animation layers rather than a tree of custom
//! `NSView` subclasses. Compact's chrome is mostly rounded rectangles, hairline
//! borders and short text runs that animate in and out — exactly what CALayer
//! does natively and efficiently — and layers keep the whole design free of
//! `define_class!` boilerplate, so there is no ObjC subclass graph to leak
//! through. Input arrives through `NSEvent` local monitors for the same reason.
//!
//! Memory discipline applies here too: the chrome allocates its layers once at
//! construction and only mutates properties afterwards. Nothing in this crate
//! caches a decoded image per tab — a tab-strip thumbnail at 2x costs ~240 KB,
//! which is a hundred times a hibernated tab, and a hundred of them would undo
//! the entire memory story.

#![cfg(target_os = "macos")]

mod chrome;
mod overlays;
mod widgets;
mod window;

pub use window::{preflight, Shell};
