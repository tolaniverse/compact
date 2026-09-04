//! WebKit backend.
//!
//! **This is the only file in Compact that owns a web view.** Everything else
//! addresses tabs by [`TabId`]. That is deliberate: a browser's classic leak is
//! a closed tab kept alive by a stray reference, and confining every
//! `Retained<WKWebView>` to one map in one file makes that a reviewable
//! property rather than a hope.
//!
//! Hibernation uses `WKWebView.interactionState`, an opaque property-list
//! object carrying the back/forward list and scroll position. M0 measured these
//! at 640 B – 3.9 KB with scroll restored exactly, against a 50 KB gate.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSData, NSKeyedArchiver, NSString, NSURLRequest, NSURL};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};

use compact_core::tab::TabId;

use crate::{EngineError, Hibernation, Result, TabEngine};

/// A serialized tab session.
///
/// Holds the live `interactionState` object plus the byte size it archives to.
/// The size is captured at hibernation time because that is when the archive
/// already exists; recomputing it later would re-archive for nothing.
struct Session {
    state: Retained<AnyObject>,
    bytes: usize,
    /// Kept so a session that fails to restore can still be reloaded by URL.
    url: String,
}

/// WebKit-backed [`TabEngine`].
pub struct WebKitEngine {
    mtm: MainThreadMarker,
    /// Shared across every tab.
    ///
    /// This is what drives content-process sharing on modern WebKit: web views
    /// built from one configuration (and therefore one website data store) let
    /// WebKit reuse a content process for same-origin tabs rather than paying
    /// the ~20-30 MB per-process floor again for each one. `WKProcessPool` used
    /// to be the lever here and is now deprecated as a no-op, so the shared
    /// configuration is the whole mechanism.
    config: Retained<WKWebViewConfiguration>,
    /// The superview live tabs are mounted into. A web view outside a view
    /// hierarchy will not reliably run its renderer, so even the headless
    /// harness supplies one.
    container: Retained<NSView>,
    /// **The** handle map. Live views exist here and nowhere else.
    views: HashMap<TabId, Retained<WKWebView>>,
    sessions: HashMap<TabId, Session>,
    /// Last known URL per tab, so a `Cold` tab can be loaded without the core
    /// having to carry engine state back down.
    urls: HashMap<TabId, String>,
    /// Which tab is currently visible; others are detached from the hierarchy.
    visible: Option<TabId>,
}

impl WebKitEngine {
    /// Build an engine that mounts tabs into `container`.
    pub fn new(mtm: MainThreadMarker, container: Retained<NSView>) -> Self {
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        Self {
            mtm,
            config,
            container,
            views: HashMap::new(),
            sessions: HashMap::new(),
            urls: HashMap::new(),
            visible: None,
        }
    }

    /// Build an engine with its own detached container, for headless
    /// measurement runs.
    pub fn headless(mtm: MainThreadMarker) -> Self {
        let frame = CGRect::new(
            objc2_core_foundation::CGPoint::new(0.0, 0.0),
            objc2_core_foundation::CGSize::new(1280.0, 800.0),
        );
        let v = NSView::alloc(mtm);
        let container = NSView::initWithFrame(v, frame);
        Self::new(mtm, container)
    }

    pub fn container(&self) -> &NSView {
        &self.container
    }

    /// Create a web view sized to the container and mount it.
    fn make_view(&mut self, id: TabId) -> Retained<WKWebView> {
        let frame = self.container.bounds();
        let view = unsafe {
            let v = WKWebView::alloc(self.mtm);
            WKWebView::initWithFrame_configuration(v, frame, &self.config)
        };
        // Track the container on resize; the chrome floats above the page
        // rather than insetting it, so the page is always full-bleed.
        view.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        self.container.addSubview(&view);
        self.views.insert(id, view.clone());
        view
    }

    /// Archive an `interactionState` to measure it.
    ///
    /// The property is documented as property-list-compatible, so it archives
    /// cleanly; failure here is not fatal, it just costs us the size reading.
    fn archive_size(state: &AnyObject) -> usize {
        unsafe {
            let data: Option<Retained<NSData>> =
                NSKeyedArchiver::archivedDataWithRootObject_requiringSecureCoding_error(
                    state, false,
                )
                .ok();
            data.map(|d| d.len()).unwrap_or(0)
        }
    }

    /// Show one tab and detach the rest from the view hierarchy.
    ///
    /// Detaching rather than merely hiding matters: an attached-but-hidden web
    /// view keeps compositing resources alive.
    pub fn show(&mut self, id: TabId) {
        if self.visible == Some(id) {
            return;
        }
        if let Some(prev) = self.visible.take() {
            if let Some(v) = self.views.get(&prev) {
                v.removeFromSuperview();
            }
        }
        if let Some(v) = self.views.get(&id) {
            v.setFrame(self.container.bounds());
            self.container.addSubview(v);
            self.visible = Some(id);
        }
    }

    /// The live view for a tab, for the UI layer to size or focus.
    pub fn view(&self, id: TabId) -> Option<&WKWebView> {
        self.views.get(&id).map(|v| &**v)
    }

    /// Current page title, if the tab is live and has one.
    pub fn title(&self, id: TabId) -> Option<String> {
        let v = self.views.get(&id)?;
        unsafe { v.title() }.map(|t| t.to_string()).filter(|t| !t.is_empty())
    }

    /// Current URL, if the tab is live.
    pub fn url(&self, id: TabId) -> Option<String> {
        let v = self.views.get(&id)?;
        unsafe { v.URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string())
    }
}

impl TabEngine for WebKitEngine {
    fn capability(&self) -> Hibernation {
        Hibernation::Blob
    }

    fn load(&mut self, id: TabId, url: &str) -> Result<()> {
        let view = match self.views.get(&id) {
            Some(v) => v.clone(),
            None => self.make_view(id),
        };
        let ns = NSString::from_str(url);
        let nsurl = NSURL::URLWithString(&ns)
            .ok_or_else(|| EngineError::BadUrl(url.to_string()))?;
        let req = NSURLRequest::requestWithURL(&nsurl);
        unsafe { view.loadRequest(&req) };
        self.urls.insert(id, url.to_string());
        Ok(())
    }

    fn hibernate(&mut self, id: TabId) -> Result<usize> {
        let view = self.views.remove(&id).ok_or(EngineError::UnknownTab(id))?;

        let url = self
            .url(id)
            .or_else(|| self.urls.get(&id).cloned())
            .unwrap_or_default();

        let bytes = match unsafe { view.interactionState() } {
            Some(state) => {
                let bytes = Self::archive_size(&state);
                self.sessions.insert(id, Session { state, bytes, url: url.clone() });
                bytes
            }
            // No interaction state (a tab that never navigated). Falling back
            // to a URL-only session keeps hibernation total rather than
            // stranding the tab as unrestorable.
            None => {
                self.urls.insert(id, url);
                0
            }
        };

        view.removeFromSuperview();
        // Stop any in-flight load so teardown does not race a navigation that
        // would touch a view we are dropping.
        unsafe { let _: () = msg_send![&*view, stopLoading]; }
        if self.visible == Some(id) {
            self.visible = None;
        }
        // Dropping the last `Retained` here releases the web view. WebKit parks
        // the content process in its reuse cache for ~5 s afterwards, which is
        // why measurement must sample on a schedule rather than immediately.
        drop(view);
        Ok(bytes)
    }

    fn restore(&mut self, id: TabId) -> Result<()> {
        let Some(session) = self.sessions.remove(&id) else {
            // No blob, but we may still know where the tab pointed.
            let url = self.urls.get(&id).cloned().ok_or(EngineError::NoSession(id))?;
            return self.load(id, &url);
        };
        let view = self.make_view(id);
        unsafe { view.setInteractionState(Some(&session.state)) };
        self.urls.insert(id, session.url);
        Ok(())
    }

    fn destroy(&mut self, id: TabId) {
        if let Some(view) = self.views.remove(&id) {
            view.removeFromSuperview();
        }
        self.sessions.remove(&id);
        self.urls.remove(&id);
        if self.visible == Some(id) {
            self.visible = None;
        }
    }

    fn is_live(&self, id: TabId) -> bool {
        self.views.contains_key(&id)
    }

    fn live_count(&self) -> usize {
        self.views.len()
    }

    fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn session_bytes(&self) -> usize {
        self.sessions.values().map(|s| s.bytes).sum()
    }
}
