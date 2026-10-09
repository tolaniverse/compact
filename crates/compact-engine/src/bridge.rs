//! The one ObjC class Compact defines.
//!
//! Everything else in the UI avoids subclassing — input arrives through
//! `NSEvent` monitors, and drawing is CALayers. But two things genuinely
//! require a delegate object, because WebKit has no callback-block form of
//! either:
//!
//! - **`WKScriptMessageHandler`**, so the internal pages can act. Without it a
//!   click on the new-tab page could only be noticed by polling the web view's
//!   URL, which means a settings toggle that responds a frame or two late.
//! - **`WKNavigationDelegate`**, so a failed navigation can be *explained*
//!   rather than left as WebKit's own grey error text. That is the entire
//!   premise of the error page.
//!
//! The class holds no Rust state. Events go into a main-thread-only queue and
//! the shell drains it, so the delegate never needs a handle to the app — which
//! also means it cannot keep one alive.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSError, NSString};
use objc2_web_kit::{
    WKNavigation, WKNavigationDelegate, WKScriptMessage, WKScriptMessageHandler,
    WKUserContentController, WKWebView,
};

/// The message-handler name pages post to: `window.webkit.messageHandlers.compact`.
pub const HANDLER_NAME: &str = "compact";

/// Something a web view did that the shell has to answer for.
///
/// `view` is the web view's address, which the engine maps back to a
/// [`TabId`](compact_core::tab::TabId). An address is enough because the engine
/// holds a strong reference to every live view, so one cannot be freed and its
/// address reused while an event about it is still queued.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// A `postMessage` on the internal pages' handler.
    ///
    /// `origin` is the sending frame's security origin as `protocol://host`,
    /// or empty for a subframe. Any page can post to the handler, so the
    /// shell must check it — see [`is_internal_origin`].
    Message { view: usize, body: String, origin: String },
    /// A line from the capture hook in an agent workspace's page: console
    /// output, an error or a request. The page wrote it, so it is data about
    /// the page and nothing more. See [`crate::capture`].
    Capture { view: usize, line: String },
    /// A navigation failed. `code` is the `NSError` code.
    Failed { view: usize, code: isize },
    /// The web content process backing this view died.
    Crashed { view: usize },
}

thread_local! {
    static EVENTS: RefCell<Vec<EngineEvent>> = const { RefCell::new(Vec::new()) };
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Install the callback fired after each event is queued.
///
/// The shell uses it to drain on the next run-loop turn rather than reentrantly
/// during a WebKit callback.
pub fn on_event(f: impl Fn() + 'static) {
    WAKE.with(|w| *w.borrow_mut() = Some(Box::new(f)));
}

/// Take every queued event.
pub fn drain_events() -> Vec<EngineEvent> {
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

fn push(event: EngineEvent) {
    EVENTS.with(|e| e.borrow_mut().push(event));
    WAKE.with(|w| {
        if let Some(f) = w.borrow().as_ref() {
            f();
        }
    });
}

fn addr(view: &WKWebView) -> usize {
    view as *const WKWebView as usize
}

define_class!(
    // SAFETY:
    // - `NSObject` imposes no subclassing requirements.
    // - `Bridge` holds no Rust state and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CompactBridge"]
    #[ivars = ()]
    pub struct Bridge;

    unsafe impl NSObjectProtocol for Bridge {}

    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _ucc: &WKUserContentController, message: &WKScriptMessage) {
            let body = unsafe { message.body() };
            // Pages only ever post strings. Anything else is not ours, and
            // asking an arbitrary object for `description` to find out would
            // hand a compromised page a way to feed us text.
            if !is_string(&body) {
                return;
            }
            let s: Retained<NSString> = unsafe { Retained::cast_unchecked(body) };
            let Some(view) = (unsafe { message.webView() }) else { return };
            let view = addr(&view);
            // Two handlers share this object. Which one a message came in on
            // decides what it is allowed to mean: a capture line can only
            // ever be recorded, never acted on.
            if unsafe { message.name() }.to_string() == crate::capture::CAPTURE_HANDLER {
                push(EngineEvent::Capture { view, line: s.to_string() });
            } else {
                let frame = unsafe { message.frameInfo() };
                let origin = if unsafe { frame.isMainFrame() } {
                    let o = unsafe { frame.securityOrigin() };
                    format!("{}://{}", unsafe { o.protocol() }, unsafe { o.host() })
                } else {
                    String::new()
                };
                push(EngineEvent::Message { view, body: s.to_string(), origin });
            }
        }
    }

    unsafe impl WKNavigationDelegate for Bridge {
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(
            &self,
            web_view: &WKWebView,
            _nav: Option<&WKNavigation>,
            error: &NSError,
        ) {
            push(EngineEvent::Failed { view: addr(web_view), code: error.code() });
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(
            &self,
            web_view: &WKWebView,
            _nav: Option<&WKNavigation>,
            error: &NSError,
        ) {
            push(EngineEvent::Failed { view: addr(web_view), code: error.code() });
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn did_terminate(&self, web_view: &WKWebView) {
            push(EngineEvent::Crashed { view: addr(web_view) });
        }
    }
);

impl Bridge {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Whether a message's `origin` is one of Compact's own pages.
///
/// Internal pages are loaded from a string with no base URL, which gives them
/// an opaque origin: no scheme and no host. Every site has both, and a
/// subframe reports no origin at all, so neither can pass. Measured: an
/// internal page reports `://`, a page on `localhost:8765` reports
/// `http://localhost`.
pub fn is_internal_origin(origin: &str) -> bool {
    origin == "://"
}

fn is_string(obj: &AnyObject) -> bool {
    unsafe { msg_send![obj, isKindOfClass: NSString::class()] }
}

#[cfg(test)]
mod tests {
    use super::is_internal_origin;

    #[test]
    fn only_an_opaque_main_frame_origin_is_ours() {
        assert!(is_internal_origin("://"));
        assert!(!is_internal_origin("http://localhost"));
        assert!(!is_internal_origin("https://example.com"));
        assert!(!is_internal_origin("file://"));
        assert!(!is_internal_origin(""), "a subframe");
    }
}
