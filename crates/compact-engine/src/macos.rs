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
//!
//! **Profiles.** Your tabs share one configuration and WebKit's default,
//! persistent website data store. Each agent workspace gets a configuration
//! of its own with a separate, non-persistent store, so an agent never sees
//! your cookies and two agents never see each other's. Agent pages also carry
//! the capture hook ([`crate::capture`]) and are inspectable from Safari's
//! Web Inspector.

use std::cell::Cell;
use std::collections::HashMap;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{msg_send, sel, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSImage, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSData, NSDictionary, NSError, NSKeyedArchiver, NSString, NSURLRequest, NSURL};
use objc2_web_kit::{
    WKContentWorld, WKSnapshotConfiguration, WKUserScript, WKUserScriptInjectionTime, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore,
};

use crate::bridge::{Bridge, HANDLER_NAME};
use crate::capture::{Capture, CAPTURE_HANDLER, CAPTURE_SCRIPT};
use crate::inspect;

use compact_core::tab::TabId;
use compact_core::workspace::Profile;

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
    /// The script-message handler and navigation delegate, shared by every web
    /// view. One object rather than one per tab: it is stateless, and the
    /// events it emits already say which view they came from.
    bridge: Retained<Bridge>,
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
    /// Whose data store each tab's view is built with. Absent means yours.
    profiles: HashMap<TabId, Profile>,
    /// One configuration per agent profile, made on first use and dropped
    /// with that profile's last tab.
    agent_configs: HashMap<String, Retained<WKWebViewConfiguration>>,
    /// Console and network activity of agent tabs while they are live.
    captures: HashMap<TabId, Capture>,
    /// Where inspection scripts run, out of the page's reach.
    world: Retained<WKContentWorld>,
    /// The developer tools drawer's own web view, while it is open. Never a
    /// tab: it holds nothing but Compact's drawer page.
    tools: Option<Retained<WKWebView>>,
    /// Where pages go when something else, such as the drawer, has part of
    /// the container. `None` means all of it.
    page_frame: Option<CGRect>,
}

impl WebKitEngine {
    /// Build an engine that mounts tabs into `container`.
    pub fn new(mtm: MainThreadMarker, container: Retained<NSView>) -> Self {
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        let bridge = Bridge::new(mtm);
        // Registered on the shared configuration, so every web view built from
        // it can talk back without per-tab setup.
        unsafe {
            let ucc = config.userContentController();
            ucc.addScriptMessageHandler_name(
                objc2::runtime::ProtocolObject::from_ref(&*bridge),
                &NSString::from_str(HANDLER_NAME),
            );
        }
        Self {
            mtm,
            config,
            bridge,
            container,
            views: HashMap::new(),
            sessions: HashMap::new(),
            urls: HashMap::new(),
            visible: None,
            profiles: HashMap::new(),
            agent_configs: HashMap::new(),
            captures: HashMap::new(),
            world: unsafe { WKContentWorld::worldWithName(&NSString::from_str(inspect::WORLD), mtm) },
            tools: None,
            page_frame: None,
        }
    }

    /// Give pages `frame` of the container, or all of it with `None`.
    pub fn set_page_frame(&mut self, frame: Option<CGRect>) {
        self.page_frame = frame;
        let f = self.page_rect();
        if let Some(v) = self.visible.and_then(|id| self.views.get(&id)) {
            v.setFrame(f);
        }
    }

    fn page_rect(&self) -> CGRect {
        self.page_frame.unwrap_or_else(|| self.container.bounds())
    }

    /// Open the drawer's view at `frame`, creating it on first use. Returns
    /// true when it was just created and will say when it is ready.
    pub fn open_tools(&mut self, frame: CGRect) -> bool {
        let fresh = self.tools.is_none();
        if fresh {
            let mtm = self.mtm;
            let config = unsafe { WKWebViewConfiguration::new(mtm) };
            let view = unsafe {
                // Its own store: the drawer keeps nothing, and shares nothing
                // with your pages or an agent's.
                config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
                config.userContentController().addScriptMessageHandler_name(
                    ProtocolObject::from_ref(&*self.bridge),
                    &NSString::from_str(crate::devtools::TOOLS_HANDLER),
                );
                let v = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
                v.loadHTMLString_baseURL(&NSString::from_str(&crate::devtools::devtools_page()), None);
                v
            };
            self.tools = Some(view);
        }
        if let Some(v) = &self.tools {
            v.setFrame(frame);
            if unsafe { v.superview() }.is_none() {
                self.container.addSubview(v);
            }
        }
        fresh
    }

    /// Take the drawer off screen, keeping it for when it comes back.
    pub fn hide_tools(&mut self) {
        if let Some(v) = &self.tools {
            v.removeFromSuperview();
        }
    }

    /// Close the drawer for good, releasing its view.
    pub fn close_tools(&mut self) {
        if let Some(v) = self.tools.take() {
            v.removeFromSuperview();
        }
    }

    /// Whether the keyboard is in the drawer: `window`'s first responder is
    /// the drawer's view or something inside it.
    pub fn tools_focused(&self, window: &objc2_app_kit::NSWindow) -> bool {
        let Some(tools) = &self.tools else { return false };
        let Some(r) = window.firstResponder() else { return false };
        unsafe {
            let is_view: bool = msg_send![&*r, isKindOfClass: NSView::class()];
            is_view && msg_send![&*r, isDescendantOf: &**tools]
        }
    }

    /// Whether a web view, by address, is the drawer's.
    pub fn is_tools_view(&self, view_addr: usize) -> bool {
        self.tools.as_ref().is_some_and(|v| Retained::as_ptr(v) as usize == view_addr)
    }

    /// Run one of [`crate::devtools`]'s scripts in the drawer.
    pub fn tools_eval(&self, js: &str) {
        if let Some(v) = &self.tools {
            unsafe {
                v.evaluateJavaScript_completionHandler(&NSString::from_str(js), None);
            }
        }
    }

    /// Say whose data store `id` uses. Must be called before the tab's view
    /// is first built; a live view keeps the store it was built with.
    pub fn set_profile(&mut self, id: TabId, profile: Profile) {
        match profile {
            Profile::Personal => {
                self.profiles.remove(&id);
            }
            p => {
                self.profiles.insert(id, p);
            }
        }
    }

    /// The configuration for an agent profile, built on first use.
    fn agent_config(&mut self, key: &str) -> Retained<WKWebViewConfiguration> {
        if let Some(c) = self.agent_configs.get(key) {
            return c.clone();
        }
        let mtm = self.mtm;
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe {
            config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            let ucc = config.userContentController();
            let handler = ProtocolObject::from_ref(&*self.bridge);
            // Internal pages render in agent workspaces too.
            ucc.addScriptMessageHandler_name(handler, &NSString::from_str(HANDLER_NAME));
            ucc.addScriptMessageHandler_name(handler, &NSString::from_str(CAPTURE_HANDLER));
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(CAPTURE_SCRIPT),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
            );
            ucc.addUserScript(&script);
            // The review overlay, dormant until switched on, and the handler
            // it posts to. Both live in Compact's world only, so a page can
            // neither drive the overlay nor post a comment of its own.
            let review = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &NSString::from_str(crate::review::REVIEW_SCRIPT),
                WKUserScriptInjectionTime::AtDocumentEnd,
                true,
                &self.world,
            );
            ucc.addUserScript(&review);
            // The page-world half of source lookup: it only answers the
            // overlay's question about one element.
            let hook = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(crate::source::SOURCE_HOOK),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
            );
            ucc.addUserScript(&hook);
            ucc.addScriptMessageHandler_contentWorld_name(
                handler,
                &self.world,
                &NSString::from_str(crate::review::REVIEW_HANDLER),
            );
        }
        self.agent_configs.insert(key.to_string(), config.clone());
        config
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
        let agent = match self.profiles.get(&id).cloned() {
            Some(Profile::Agent(key)) => Some(self.agent_config(&key)),
            _ => None,
        };
        let config = agent.as_ref().unwrap_or(&self.config);
        let view = unsafe {
            let v = WKWebView::alloc(self.mtm);
            WKWebView::initWithFrame_configuration(v, frame, config)
        };
        if agent.is_some() {
            self.captures.entry(id).or_default();
            // So Safari's Web Inspector can attach for anything the capture
            // hook cannot see. The property arrived in macOS 13.3.
            unsafe {
                let can: bool = msg_send![&*view, respondsToSelector: sel!(setInspectable:)];
                if can {
                    view.setInspectable(true);
                }
            }
        }
        unsafe {
            view.setNavigationDelegate(Some(objc2::runtime::ProtocolObject::from_ref(
                &*self.bridge,
            )));
        }
        // Track the container on resize; the chrome floats above the page
        // rather than insetting it, so the page is always full-bleed.
        view.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        // Deliberately *not* mounted here. Only `show` puts a view in the
        // hierarchy: a tab created in the background — a prewarm, or a restore
        // the user has not switched to — would otherwise be added on top of the
        // visible page and silently replace it. Keeping exactly one view
        // mounted also avoids paying compositing resources for tabs nobody is
        // looking at.
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
        // No early return on `visible == id`: the view may have been rebuilt by
        // a restore since it was last mounted, so re-mounting is the only way
        // to be sure the hierarchy holds the current view for this tab.
        if let Some(prev) = self.visible.take().filter(|p| *p != id) {
            if let Some(v) = self.views.get(&prev) {
                v.removeFromSuperview();
            }
        }
        let frame = self.page_rect();
        if let Some(v) = self.views.get(&id) {
            v.setFrame(frame);
            self.container.addSubview(v);
            self.visible = Some(id);
        }
    }

    /// Which tab is currently mounted, if any.
    pub fn visible(&self) -> Option<TabId> {
        self.visible
    }

    /// Mount `id` alongside whatever is already showing, without unmounting it.
    ///
    /// Used for the workspace transition, which needs both pages on screen at
    /// once. `unmount` ends it.
    pub fn mount_alongside(&mut self, id: TabId) {
        if let Some(v) = self.views.get(&id) {
            v.setFrame(self.container.bounds());
            self.container.addSubview(v);
            self.visible = Some(id);
        }
    }

    /// Detach a tab's view from the hierarchy without destroying it.
    pub fn unmount(&self, id: TabId) {
        if let Some(v) = self.views.get(&id) {
            v.removeFromSuperview();
        }
    }

    /// The live view for a tab, for the UI layer to size or focus.
    pub fn view(&self, id: TabId) -> Option<&WKWebView> {
        self.views.get(&id).map(|v| &**v)
    }

    /// Which tab a web view belongs to, by address.
    ///
    /// The delegate reports a `WKWebView`, and everything above the engine
    /// speaks [`TabId`]. This is the only translation between the two.
    pub fn tab_of(&self, view_addr: usize) -> Option<TabId> {
        self.views
            .iter()
            .find(|(_, v)| Retained::as_ptr(v) as usize == view_addr)
            .map(|(id, _)| *id)
    }

    /// Render markup we generated into a tab, creating its view if needed.
    ///
    /// The base URL is deliberately `nil`, which gives the document a unique
    /// opaque origin: no network reach, no site's storage, and nothing on the
    /// web able to reach it. The internal pages need none of that — they talk
    /// to the app over the message handler and nowhere else.
    ///
    /// It must not be `compact://…`. WebKit does not recognise the scheme, so
    /// it hands the URL to LaunchServices, which answers with a system alert
    /// asking which application should open it. `id`'s own address stays
    /// `compact://newtab` in the tab model, which is what the address bar
    /// reads — the web view simply never sees it.
    pub fn load_html(&mut self, id: TabId, html: &str, address: &str) -> Result<()> {
        let view = match self.views.get(&id) {
            Some(v) => v.clone(),
            None => self.make_view(id),
        };
        let ns = NSString::from_str(html);
        unsafe { view.loadHTMLString_baseURL(&ns, None) };
        self.urls.insert(id, address.to_string());
        Ok(())
    }

    /// Reload whatever the tab is already showing.
    pub fn reload(&mut self, id: TabId) {
        if let Some(v) = self.views.get(&id) {
            unsafe {
                let _: Option<Retained<objc2_web_kit::WKNavigation>> = msg_send![&**v, reload];
            }
        }
    }

    /// Whether a navigation is in flight, and how far along it is.
    ///
    /// `estimatedProgress` is WebKit's own figure, so the load bar reflects the
    /// actual navigation rather than a timer pretending to.
    pub fn load_state(&self, id: TabId) -> Option<(bool, f64)> {
        let v = self.views.get(&id)?;
        unsafe { Some((v.isLoading(), v.estimatedProgress())) }
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

// ---- eyes: what agent tools read -------------------------------------------

impl WebKitEngine {
    /// Record a line from an agent page's capture hook.
    pub fn ingest(&mut self, view_addr: usize, line: &str) {
        let Some(id) = self.tab_of(view_addr) else { return };
        // Only tabs built with an agent profile have a buffer, so a line that
        // somehow arrives from one of your tabs is dropped.
        if let Some(c) = self.captures.get_mut(&id) {
            c.ingest(line);
        }
    }

    /// A live agent tab's captured console and network activity.
    pub fn capture(&self, id: TabId) -> Option<&Capture> {
        self.captures.get(&id)
    }

    /// The page as an outline of roles, names and element refs. See
    /// [`inspect::SNAPSHOT_SCRIPT`].
    pub fn snapshot(&self, id: TabId, done: impl FnOnce(std::result::Result<String, String>) + 'static) {
        match self.views.get(&id) {
            Some(v) => self.eval_in_world(v, &inspect::snapshot_script(), done),
            None => done(Err(format!("{id} is not live"))),
        }
    }

    /// A PNG of the visible page, or of one element scrolled into view.
    ///
    /// `target` is a ref from [`Self::snapshot`] (`e12`) or a CSS selector.
    pub fn screenshot(
        &self,
        id: TabId,
        target: Option<&str>,
        done: impl FnOnce(std::result::Result<Vec<u8>, String>) + 'static,
    ) {
        let Some(view) = self.views.get(&id).cloned() else {
            done(Err(format!("{id} is not live")));
            return;
        };
        let Some(target) = target else {
            self.clean_snapshot(view, None, done);
            return;
        };
        let mtm = self.mtm;
        let world = self.world.clone();
        let v = view.clone();
        let target = target.to_string();
        self.eval_in_world(&view, &inspect::locate_script(&target), move |r| match r {
            Err(e) => done(Err(e)),
            Ok(s) if s == inspect::NO_MATCH => {
                done(Err(format!("no element matches {target}")))
            }
            Ok(s) => {
                let b = v.bounds();
                match inspect::parse_rect(&s, (b.size.width, b.size.height)) {
                    Some(rect) => clean_snapshot_with(mtm, world, v, Some(rect), done),
                    None => done(Err("the element has no visible area".into())),
                }
            }
        });
    }

    /// Show or hide the review overlay on `id`, with `pins_json` as its pins.
    /// A tab that is not live, or whose page has no overlay, is left alone:
    /// the overlay asks to be switched on again when its page loads.
    pub fn set_review(&self, id: TabId, on: bool, pins_json: &str, agent: &str) {
        if let Some(v) = self.views.get(&id) {
            self.eval_in_world(v, &crate::review::set_script(on, pins_json, agent), |_| {});
        }
    }

    /// Measure `id`'s page load and rendering (see
    /// [`crate::devtools::PERF_SCRIPT`]); the answer is a JSON string.
    pub fn performance(&self, id: TabId, done: impl FnOnce(std::result::Result<String, String>) + 'static) {
        match self.views.get(&id) {
            Some(v) => self.call_async_in_world(v, crate::devtools::PERF_SCRIPT, done),
            None => done(Err(format!("{id} is not live"))),
        }
    }

    /// Run an async function body in Compact's world and hand back the
    /// string it resolves to. Unlike [`Self::eval_in_world`], this waits for
    /// promises, which is what lets a script wait for a frame to be drawn.
    fn call_async_in_world(
        &self,
        view: &WKWebView,
        body: &str,
        done: impl FnOnce(std::result::Result<String, String>) + 'static,
    ) {
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |res: *mut AnyObject, err: *mut NSError| {
            let Some(done) = done.take() else { return };
            if let Some(err) = unsafe { err.as_ref() } {
                done(Err(js_error_text(err)));
                return;
            }
            match unsafe { res.as_ref() } {
                Some(obj) if is_string(obj) => {
                    let s: &NSString = unsafe { &*(obj as *const AnyObject as *const NSString) };
                    done(Ok(s.to_string()));
                }
                _ => done(Err("the script returned no text".into())),
            }
        });
        unsafe {
            view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(body),
                None,
                None,
                &self.world,
                Some(&block),
            );
        }
    }

    /// Snapshot `view` with Compact's own page UI — the review overlay and
    /// the drawer — out of the picture, then put it back.
    fn clean_snapshot(
        &self,
        view: Retained<WKWebView>,
        rect: Option<(f64, f64, f64, f64)>,
        done: impl FnOnce(std::result::Result<Vec<u8>, String>) + 'static,
    ) {
        clean_snapshot_with(self.mtm, self.world.clone(), view, rect, done);
    }

    /// A PNG of part of the viewport, in CSS pixels, clipped to it.
    pub fn screenshot_rect(
        &self,
        id: TabId,
        rect: (f64, f64, f64, f64),
        done: impl FnOnce(std::result::Result<Vec<u8>, String>) + 'static,
    ) {
        let Some(view) = self.views.get(&id) else {
            done(Err(format!("{id} is not live")));
            return;
        };
        let b = view.bounds();
        let s = format!("{},{},{},{}", rect.0, rect.1, rect.2, rect.3);
        match inspect::parse_rect(&s, (b.size.width, b.size.height)) {
            Some(r) => self.clean_snapshot(view.clone(), Some(r), done),
            None => done(Err("that area is off screen".into())),
        }
    }

    /// Run `js` in Compact's own content world and hand back its string
    /// result. Anything other than a string is an error, by design: the
    /// scripts are ours, and a different type means something went wrong.
    fn eval_in_world(
        &self,
        view: &WKWebView,
        js: &str,
        done: impl FnOnce(std::result::Result<String, String>) + 'static,
    ) {
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |res: *mut AnyObject, err: *mut NSError| {
            let Some(done) = done.take() else { return };
            if let Some(err) = unsafe { err.as_ref() } {
                done(Err(js_error_text(err)));
                return;
            }
            match unsafe { res.as_ref() } {
                Some(obj) if is_string(obj) => {
                    let s: &NSString = unsafe { &*(obj as *const AnyObject as *const NSString) };
                    done(Ok(s.to_string()));
                }
                _ => done(Err("the script returned no text".into())),
            }
        });
        unsafe {
            view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(js),
                None,
                &self.world,
                Some(&block),
            );
        }
    }
}

/// [`WebKitEngine::clean_snapshot`] for callers that no longer hold the
/// engine, such as a script's completion handler.
fn clean_snapshot_with(
    mtm: MainThreadMarker,
    world: Retained<WKContentWorld>,
    view: Retained<WKWebView>,
    rect: Option<(f64, f64, f64, f64)>,
    done: impl FnOnce(std::result::Result<Vec<u8>, String>) + 'static,
) {
    let done = Cell::new(Some(done));
    let v = view.clone();
    let w = world.clone();
    let block = RcBlock::new(move |_res: *mut AnyObject, _err: *mut NSError| {
        let Some(done) = done.take() else { return };
        let v2 = v.clone();
        let w2 = w.clone();
        take_snapshot(mtm, &v, rect, move |shot| {
            unsafe {
                v2.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                    &NSString::from_str(crate::devtools::SHOW_UI_SCRIPT),
                    None,
                    &w2,
                    None,
                );
            }
            done(shot);
        });
    });
    unsafe {
        view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(crate::devtools::HIDE_UI_SCRIPT),
            None,
            None,
            &world,
            Some(&block),
        );
    }
}

fn take_snapshot(
    mtm: MainThreadMarker,
    view: &WKWebView,
    rect: Option<(f64, f64, f64, f64)>,
    done: impl FnOnce(std::result::Result<Vec<u8>, String>) + 'static,
) {
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    if let Some((x, y, w, h)) = rect {
        unsafe { config.setRect(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))) };
    }
    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |img: *mut NSImage, err: *mut NSError| {
        let Some(done) = done.take() else { return };
        if let Some(err) = unsafe { err.as_ref() } {
            done(Err(err.localizedDescription().to_string()));
            return;
        }
        match unsafe { img.as_ref() }.and_then(png_of) {
            Some(png) => done(Ok(png)),
            None => done(Err("the snapshot could not be encoded".into())),
        }
    });
    unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
}

/// Encode an image as PNG, by way of its TIFF representation, which avoids
/// pulling Core Graphics image types into the engine.
fn png_of(img: &NSImage) -> Option<Vec<u8>> {
    unsafe {
        let tiff: Option<Retained<NSData>> = msg_send![img, TIFFRepresentation];
        let tiff = tiff?;
        let cls = objc2::runtime::AnyClass::get(c"NSBitmapImageRep")?;
        let rep: Option<Retained<AnyObject>> = msg_send![cls, imageRepWithData: &*tiff];
        let rep = rep?;
        let props = NSDictionary::<NSString, AnyObject>::new();
        // NSBitmapImageFileTypePNG.
        let png: Option<Retained<NSData>> =
            msg_send![&*rep, representationUsingType: 4usize, properties: &*props];
        png.map(|d| d.to_vec())
    }
}

/// What a failed script evaluation says, preferring the JavaScript exception
/// message over WebKit's generic "A JavaScript exception occurred".
fn js_error_text(err: &NSError) -> String {
    unsafe {
        let info = err.userInfo();
        let key = NSString::from_str("WKJavaScriptExceptionMessage");
        let msg: Option<Retained<AnyObject>> = msg_send![&*info, objectForKey: &*key];
        match msg {
            Some(m) if is_string(&m) => {
                let s: &NSString = &*(Retained::as_ptr(&m) as *const NSString);
                s.to_string()
            }
            _ => err.localizedDescription().to_string(),
        }
    }
}

fn is_string(obj: &AnyObject) -> bool {
    unsafe { msg_send![obj, isKindOfClass: NSString::class()] }
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
        // Captured activity is not part of the session: it costs memory, and
        // a hibernated tab is supposed to cost almost none.
        self.captures.remove(&id);

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
        self.captures.remove(&id);
        if self.visible == Some(id) {
            self.visible = None;
        }
        // An agent profile's store goes with its last tab, taking the
        // agent's cookies with it.
        if let Some(Profile::Agent(key)) = self.profiles.remove(&id) {
            let in_use = self.profiles.values().any(|p| *p == Profile::Agent(key.clone()));
            if !in_use {
                self.agent_configs.remove(&key);
            }
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
