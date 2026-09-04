//! The application shell: window, engine, event routing, render loop.
//!
//! Input arrives through `NSEvent` local monitors rather than through
//! `NSResponder` overrides, which keeps the entire UI free of ObjC subclassing.
//! Shared state lives in one `Rc<RefCell<Inner>>` that the monitor blocks
//! capture; everything here is main-thread-only, so no locking is involved.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAnimatablePropertyContainer,
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSEventMask, NSEventModifierFlags, NSEventType, NSView, NSWindow, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSString, NSTimer};

use compact_core::command::{resolve, Action, Mods};
use compact_core::policy::LiveBudget;
use compact_core::tab::TabId;
use compact_core::theme::{bg, radius};
use compact_core::{App, Effect, Overlay};
use compact_engine::{TabEngine, WebKitEngine};
use compact_mem::{Guard, PressureMonitor};

use crate::chrome::{Chrome, Hit};
use crate::overlays::{Overlays, SettingsHit};
use crate::widgets::{animate_with, queue_after, rect, without_animation};

struct Inner {
    app: App,
    engine: WebKitEngine,
    chrome: Chrome,
    overlays: Overlays,
    window: Retained<NSWindow>,
    page: Retained<NSView>,
    started: Instant,
    pressure: Option<std::sync::Arc<compact_mem::pressure::PressureCell>>,
    /// WebKit helpers that existed before we started, so the footprint we
    /// report is ours and not some other browser's.
    census: compact_mem::WebKitCensus,
    /// Weak self-handle, so deferred work can reach back without keeping the
    /// shell alive past the window.
    self_ref: std::rc::Weak<RefCell<Inner>>,
    /// True while a workspace transition is travelling, so `render` leaves the
    /// pages where the animation put them.
    sliding: bool,
    /// Focus requested by the last action, applied after the render that makes
    /// the target visible. ⌘L reveals the address bar and focuses it in one
    /// step; focusing first would target a hidden field.
    pending_focus: Option<Focus>,
    /// Tabs whose title we have already adopted at least once.
    ///
    /// A tab's *first* title is just its page loading, which is not news — and
    /// with background workspaces prewarmed, notifying on it would greet the
    /// user with a toast per tab at launch. Only later changes are events.
    titled: std::collections::HashSet<TabId>,
}

impl Inner {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Window size in points.
    fn size(&self) -> (f64, f64) {
        let b = self.page.bounds();
        (b.size.width, b.size.height)
    }

    /// Carry out the engine work the core asked for.
    fn perform(&mut self, effects: Vec<Effect>) {
        // Direction, plus whether the destination was already painted.
        let mut slide: Option<(i8, bool)> = None;
        for e in effects {
            match e {
                Effect::Load(id, url) => {
                    let _ = self.engine.load(id, &url);
                }
                Effect::Restore(id) => {
                    let _ = self.engine.restore(id);
                }
                Effect::Hibernate(id) => {
                    // A failed hibernate leaves the tab live; the next
                    // reconcile will try again rather than losing the tab.
                    let _ = self.engine.hibernate(id);
                }
                Effect::Destroy(id) => self.engine.destroy(id),
                // Deferred: the render that follows is what makes these
                // visible, and focusing a hidden field does nothing.
                Effect::FocusAddressField => self.pending_focus = Some(Focus::Address),
                Effect::FocusPaletteField => self.pending_focus = Some(Focus::Palette),
                Effect::SlideWorkspace(dir) => {
                    // Sampled here, before the tab's own Load/Restore effects
                    // run, so it reflects whether the destination already had a
                    // rendered page rather than one we are about to request.
                    let target = self.app.workspaces.current().active;
                    slide = Some((dir, self.engine.is_live(target)));
                }
            }
        }
        // Whatever just happened, the visible tab may have changed.
        let active = self.app.workspaces.current().active;
        match slide {
            // Sliding a blank page in is worse than not sliding: the outgoing
            // page leaves, nothing visibly arrives, and the transition reads as
            // a glitch. Prewarming means this is the uncommon case.
            Some((dir, true)) if self.app.chrome.smooth_motion => {
                self.slide_workspace(active, dir)
            }
            _ => self.engine.show(active),
        }
    }

    /// The workspace transition: the outgoing page leaves in the direction of
    /// travel and the incoming one arrives from the opposite side.
    ///
    /// Animates view `frame` and `alphaValue` through `NSAnimationContext`,
    /// not layer `transform`. A layer-backed `NSView` answers `actionForKey:`
    /// with `NSNull`, so implicit `CALayer` animations on its backing layer are
    /// suppressed and every value simply jumps — which is what an earlier
    /// version of this did.
    ///
    /// Both pages are real, mounted web views; no snapshot is taken. Snapshots
    /// would only help the incoming side, and that is exactly where they are
    /// unavailable: a hibernated tab has nothing rendered to capture, and
    /// keeping one per tab would mean a full-window image against a 687 B
    /// session.
    fn slide_workspace(&mut self, active: TabId, dir: i8) {
        use compact_core::theme::duration as dur;
        let outgoing = self.engine.visible();
        if outgoing == Some(active) || outgoing.is_none() {
            self.engine.show(active);
            return;
        }
        let (w, h) = self.size();
        let travel = w * 0.15;
        let d = dir as f64;
        let full = rect(0.0, 0.0, w, h);

        // Mount the incoming page beside the outgoing one, parked on the side
        // it should arrive from.
        self.engine.mount_alongside(active);
        let Some(incoming) = self.engine.view(active).map(|v| v.retain()) else {
            self.engine.show(active);
            return;
        };
        without_animation(|| {
            incoming.setFrame(rect(d * travel, 0.0, w, h));
            incoming.setAlphaValue(0.0);
        });

        // Hold off `render`'s frame reset until the travel is over, or it will
        // snap the incoming page to centre on the very next tick.
        self.sliding = true;

        if let Some(prev) = outgoing.and_then(|id| self.engine.view(id)).map(|v| v.retain()) {
            animate_with(compact_core::theme::EASE_SLIDE, dur::SLIDE_OUT, || {
                prev.animator().setFrame(rect(-d * travel, 0.0, w, h));
                prev.animator().setAlphaValue(0.0);
            });
        }
        animate_with(compact_core::theme::EASE_SLIDE, dur::SLIDE_IN, || {
            incoming.animator().setFrame(full);
            incoming.animator().setAlphaValue(1.0);
        });

        // Detach the outgoing page once it is off screen and restore its
        // geometry, so it is centred and opaque the next time it is shown.
        let me = self.self_ref.clone();
        queue_after(dur::SLIDE_IN, move || {
            let Some(inner) = me.upgrade() else { return };
            let mut i = inner.borrow_mut();
            if let Some(prev) = outgoing {
                if let Some(v) = i.engine.view(prev).map(|v| v.retain()) {
                    without_animation(|| {
                        v.setFrame(full);
                        v.setAlphaValue(1.0);
                    });
                }
                i.engine.unmount(prev);
            }
            i.sliding = false;
            i.render();
        });
    }

    /// Push state into the layer tree.
    fn render(&mut self) {
        let size = self.size();
        self.chrome.set_size(size);
        self.overlays.set_size(size);
        let now = self.now_ms();
        self.chrome.sync(&self.app, now);
        self.overlays.sync(&self.app);
        // Chrome floats above a full-bleed page, so the web view always fills
        // the window regardless of what is revealed. Skipped mid-transition, or
        // it would snap the travelling pages back to centre.
        if !self.sliding {
            if let Some(v) = self.engine.view(self.app.workspaces.current().active) {
                v.setFrame(rect(0.0, 0.0, size.0, size.1));
            }
        }
    }

    fn dispatch(&mut self, action: Action) {
        let fx = self.app.apply(action);
        self.settle(fx);
    }

    /// Carry out effects, redraw, then hand over the keyboard.
    ///
    /// The order is load-bearing and easy to get wrong piecemeal, so every path
    /// that produces effects goes through here rather than repeating it.
    fn settle(&mut self, fx: Vec<Effect>) {
        self.perform(fx);
        self.render();
        self.apply_focus();
    }

    /// Hand the keyboard to whatever the last action asked for, now that the
    /// render has made it visible.
    fn apply_focus(&mut self) {
        match self.pending_focus.take() {
            None => {}
            Some(Focus::Address) => self.chrome.focus_address(&self.window),
            Some(Focus::Palette) => {
                let f = self.overlays.palette_field.clone();
                f.setStringValue(&NSString::from_str(""));
                self.window.makeFirstResponder(Some(&f));
            }
        }
    }
}

pub struct Shell;

impl Shell {
    /// Build and run Compact. Returns when the app terminates.
    pub fn run(mtm: MainThreadMarker) -> Result<(), String> {
        let ns_app = NSApplication::sharedApplication(mtm);
        ns_app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        install_menu(mtm, &ns_app);

        let frame = rect(0.0, 0.0, 1480.0, 940.0);
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("Compact"));
        // The design has no title bar: chrome tears in over the page and
        // the traffic lights sit directly on it.
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setMovableByWindowBackground(true);
        window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            bg::WINDOW.r,
            bg::WINDOW.g,
            bg::WINDOW.b,
            1.0,
        )));
        window.setAcceptsMouseMovedEvents(true);
        window.center();

        let content = window.contentView().ok_or("window has no content view")?;
        content.setWantsLayer(true);
        unsafe {
            let l: *mut objc2::runtime::AnyObject = msg_send![&*content, layer];
            let _: () = msg_send![l, setCornerRadius: radius::WINDOW];
            let _: () = msg_send![l, setMasksToBounds: true];
            crate::widgets::set_host_fill(l, bg::PAGE);
        }

        // Page host: the web views mount here, below all chrome.
        let page = crate::widgets::container(mtm, content.bounds());
        page.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&page);

        let engine = WebKitEngine::new(mtm, page.clone());
        let size = (frame.size.width, frame.size.height);
        let chrome = Chrome::new(mtm, size);
        let overlays = Overlays::new(mtm);

        for v in chrome.views().into_iter().chain(overlays.views()) {
            content.addSubview(&v);
        }

        let mut app = App::new();
        // Size the live-set budget to what this machine actually has free.
        if let Some(mem) = compact_mem::guard::SystemMemory::read() {
            app.budget = LiveBudget::from_available(mem.available);
        }

        let inner = Rc::new(RefCell::new(Inner {
            app,
            engine,
            chrome,
            overlays,
            window: window.clone(),
            page,
            started: Instant::now(),
            pressure: None,
            // Snapshot before the first web view exists — helpers are launchd
            // XPC services with ppid 1 and cannot be attributed by parentage.
            census: compact_mem::WebKitCensus::baseline(),
            self_ref: std::rc::Weak::new(),
            sliding: false,
            pending_focus: None,
            titled: std::collections::HashSet::new(),
        }));
        inner.borrow_mut().self_ref = Rc::downgrade(&inner);

        // Memory pressure: shed live tabs when the system asks.
        if let Some((monitor, cell)) = PressureMonitor::start(None) {
            inner.borrow_mut().pressure = Some(cell);
            // Leak the monitor: it must outlive the run loop, and the process
            // exiting is what tears it down.
            std::mem::forget(monitor);
        }

        // Load the workspace's active tab, then let the budget fill the rest.
        {
            let mut i = inner.borrow_mut();
            let active = i.app.workspaces.current().active;
            let url = i.app.workspaces.current().active_tab().url.clone();
            i.app.workspaces.set_state(active, compact_core::tab::TabState::Live);
            let mut fx = vec![Effect::Load(active, url)];
            fx.extend(i.app.reconcile());
            i.settle(fx);
        }

        install_monitors(&inner);
        install_ticker(&inner);
        install_load_poll(&inner);


        window.makeKeyAndOrderFront(None);
        ns_app.activate();
        ns_app.run();
        Ok(())
    }
}

/// Convert an event's window location to top-left-origin window coordinates.
fn point_of(inner: &Inner, event: &NSEvent) -> (f64, f64) {
    let p = event.locationInWindow();
    let (_, h) = inner.size();
    (p.x, h - p.y)
}

fn mods_of(event: &NSEvent) -> Mods {
    let f = event.modifierFlags();
    Mods {
        cmd: f.contains(NSEventModifierFlags::Command)
            || f.contains(NSEventModifierFlags::Control),
        shift: f.contains(NSEventModifierFlags::Shift),
        alt: f.contains(NSEventModifierFlags::Option),
    }
}

fn install_monitors(inner: &Rc<RefCell<Inner>>) {
    // ---- keyboard ----
    let keys = inner.clone();
    let key_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event = unsafe { event.as_ref() };
        let mut i = keys.borrow_mut();

        let chars = event.charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let mods = mods_of(event);

        // A focused text field owns most keys; only the bindings that must work
        // while typing are intercepted ahead of it.
        let editing = i.app.overlay == Overlay::Palette || i.chrome.address_editing();

        // Palette navigation.
        if i.app.overlay == Overlay::Palette {
            match chars.as_str() {
                "\u{f701}" => {
                    i.app.move_selection(1);
                    i.render();
                    return std::ptr::null_mut();
                }
                "\u{f700}" => {
                    i.app.move_selection(-1);
                    i.render();
                    return std::ptr::null_mut();
                }
                "\r" | "\u{3}" => {
                    let q = i.overlays.palette_field.stringValue().to_string();
                    i.app.set_query(q);
                    let fx = i.app.run_selected();
                    i.settle(fx);
                    return std::ptr::null_mut();
                }
                _ => {}
            }
            // Let the field receive the keystroke, then re-filter from it.
            let pending = keys.clone();
            queue_after(0.0, move || {
                let mut i = pending.borrow_mut();
                let q = i.overlays.palette_field.stringValue().to_string();
                i.app.set_query(q);
                i.render();
            });
        }

        // Address bar commit.
        if editing && (chars == "\r" || chars == "\u{3}") && i.app.overlay != Overlay::Palette {
            let typed = i.chrome.address_text();
            let fx = i.app.navigate(&typed);
            i.perform(fx);
            // Give the keyboard back to the page; leaving it in the field means
            // the next keystroke edits the URL instead of reaching the site.
            i.window.makeFirstResponder(None);
            i.render();
            i.apply_focus();
            return std::ptr::null_mut();
        }

        let key = match chars.as_str() {
            "\u{1b}" => "Escape".to_string(),
            other => other.to_string(),
        };
        if let Some(action) = resolve(&key, mods, editing) {
            if action == Action::Dismiss {
                i.window.makeFirstResponder(None);
            }
            i.dispatch(action);
            return std::ptr::null_mut();
        }

        // Not ours — hand it back so the focused field can have it.
        event as *const NSEvent as *mut NSEvent
    });

    // ---- cursor ----
    let moves = inner.clone();
    let move_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event = unsafe { event.as_ref() };
        let mut i = moves.borrow_mut();
        let (x, y) = point_of(&i, event);
        let size = i.size();
        let revealed = i.app.on_cursor(Some((x, y)), size);
        // Hover is only meaningful over visible chrome; hit-testing hidden
        // chrome would tint rows nobody can see.
        let hit = i.chrome.hit(&i.app, x, y);
        let hovered = i.chrome.set_hover(hit);
        if revealed || hovered {
            i.render();
        }
        event as *const NSEvent as *mut NSEvent
    });

    // ---- clicks ----
    let clicks = inner.clone();
    let click_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event = unsafe { event.as_ref() };
        let mut i = clicks.borrow_mut();
        let (x, y) = point_of(&i, event);

        match i.app.overlay {
            Overlay::Settings => {
                match i.overlays.settings_hit(&i.app, x, y) {
                    Some(SettingsHit::Close) => i.dispatch(Action::Dismiss),
                    Some(SettingsHit::Layout(l)) => {
                        i.app.chrome.layout = l;
                        i.render();
                    }
                    Some(SettingsHit::Threshold(v)) => {
                        i.app.chrome.set_threshold(v);
                        i.render();
                    }
                    Some(SettingsHit::Toggle(n)) => {
                        match n {
                            0 => i.app.chrome.pinned = !i.app.chrome.pinned,
                            1 => i.app.chrome.smooth_motion = !i.app.chrome.smooth_motion,
                            _ => i.app.chrome.dim_page = !i.app.chrome.dim_page,
                        }
                        i.render();
                    }
                    Some(SettingsHit::Chrome) => {}
                    // Outside the drawer: dismiss, matching the scrim.
                    None => i.dispatch(Action::Dismiss),
                }
                return std::ptr::null_mut();
            }
            Overlay::Switcher => {
                match i.overlays.switcher_hit(&i.app, x, y) {
                    Some(idx) => i.dispatch(Action::SwitchWorkspace(idx)),
                    None => i.dispatch(Action::Dismiss),
                }
                return std::ptr::null_mut();
            }
            Overlay::Palette => {
                match i.overlays.palette_hit(&i.app, x, y) {
                    Some(idx) => {
                        i.app.selection = idx;
                        let fx = i.app.run_selected();
                        i.settle(fx);
                    }
                    None => i.dispatch(Action::Dismiss),
                }
                return std::ptr::null_mut();
            }
            Overlay::Shortcuts => {
                i.dispatch(Action::Dismiss);
                return std::ptr::null_mut();
            }
            Overlay::None => {}
        }

        if let Some((id, dismiss)) = i.chrome.toast_hit(&i.app, x, y) {
            if dismiss {
                i.app.notifications.dismiss(id);
                i.render();
            } else {
                // Opening a notification jumps to its workspace and focuses the
                // tab it came from.
                let fx = i.app.open_notification(id);
                i.settle(fx);
            }
            return std::ptr::null_mut();
        }

        let hit = i.chrome.hit(&i.app, x, y);

        // Clicking anywhere that is not the address bar puts it away. It floats
        // over the page, so it has to leave the moment attention does.
        let on_address = matches!(hit, Some(Hit::Address) | Some(Hit::Palette) | Some(Hit::ToggleBookmark));
        if !on_address && i.app.dismiss_address() {
            i.window.makeFirstResponder(None);
            i.render();
        }

        match hit {
            Some(Hit::SelectTab(id)) => i.dispatch(Action::SelectTab(id)),
            Some(Hit::CloseTab(id)) => i.dispatch(Action::CloseTab(id)),
            Some(Hit::NewTab) => i.dispatch(Action::NewTab),
            Some(Hit::Bookmark(b)) => i.dispatch(Action::OpenBookmark(b)),
            Some(Hit::RemoveBookmark(b)) => i.dispatch(Action::RemoveBookmark(b)),
            Some(Hit::ToggleBookmark) => i.dispatch(Action::ToggleBookmark),
            Some(Hit::ToggleLayout) => i.dispatch(Action::ToggleLayout),
            Some(Hit::Settings) => i.dispatch(Action::OpenSettings),
            Some(Hit::Palette) => i.dispatch(Action::OpenPalette),
            Some(Hit::Address) => i.dispatch(Action::FocusAddress),
            Some(Hit::Chrome) => {}
            // Not on chrome: let the page have it.
            None => return event as *const NSEvent as *mut NSEvent,
        }
        std::ptr::null_mut()
    });

    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &key_block);
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::MouseMoved,
            &move_block,
        );
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown,
            &click_block,
        );
    }
    // Monitors must outlive this function; the process owns them for its life.
    std::mem::forget(key_block);
    std::mem::forget(move_block);
    std::mem::forget(click_block);
}

/// Low-frequency housekeeping: expire toasts, follow page titles, and act on
/// memory pressure. One second is deliberate — none of this is animation, and a
/// display-linked timer would burn power to redraw text that rarely changes.
fn install_ticker(inner: &Rc<RefCell<Inner>>) {
    let tick = inner.clone();
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        let mut i = tick.borrow_mut();
        let now = i.now_ms();
        // Keep the core's clock current so toasts age and expire correctly.
        i.app.clock = now;
        let mut dirty = i.app.notifications.expire(now);

        // Adopt real page titles as navigations settle — across every
        // workspace, not just the visible one. A background workspace changing
        // under you is precisely what the notification stack is for.
        let ids: Vec<(usize, TabId)> = i
            .app
            .workspaces
            .spaces
            .iter()
            .enumerate()
            .flat_map(|(w, ws)| ws.tabs.iter().map(move |t| (w, t.id)))
            .collect();
        let current_ws = i.app.workspaces.current;
        for (ws, id) in ids {
            let Some(title) = i.engine.title(id) else { continue };
            let cur = i
                .app
                .workspaces
                .locate(id)
                .map(|(_, t)| t.title.clone())
                .unwrap_or_default();
            if cur == title {
                continue;
            }
            let url = i.engine.url(id).unwrap_or_default();
            let host = url
                .split("://")
                .nth(1)
                .unwrap_or(&url)
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            i.app.workspaces.set_location(id, &title, &host, &url);
            dirty = true;

            // `insert` reports true the first time, which is the initial load.
            let first_load = i.titled.insert(id);
            if !first_load && ws != current_ws {
                i.app.notifications.push(ws, title, host, id, now);
                i.app.workspaces.add_unread(ws);
            }
        }

        // Report what we actually cost. `phys_footprint`, never RSS: RSS counts
        // shared WebKit framework pages against every process that maps them.
        let (helper_bytes, _) = i.census.total();
        let own = compact_mem::footprint(std::process::id() as compact_mem::Pid).unwrap_or(0);
        let measured = own + helper_bytes;
        if i.app.measured_footprint != Some(measured) {
            i.app.measured_footprint = Some(measured);
            dirty = true;
        }

        if let Some(cell) = &i.pressure {
            let p = cell.get();
            if p != i.app.pressure {
                let fx = i.app.set_pressure(p);
                i.perform(fx);
                dirty = true;
            }
        }

        if dirty {
            i.render();
        }
    });
    unsafe {
        let t = NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block);
        std::mem::forget(t);
    }
    std::mem::forget(block);
}

/// Watch the visible tab's navigation.
///
/// Separate from the housekeeping tick, and much faster. Housekeeping is
/// title-following and memory sampling, where one second is generous; a
/// navigation is over in a few hundred milliseconds, so at 1 Hz the load bar
/// was usually never sampled while a page was actually loading — clicking a
/// link looked like nothing had happened.
///
/// 12 Hz is a compromise: fast enough that the bar appears within a frame or
/// two of a click, cheap enough to leave running. Each tick is two ObjC
/// property reads, and it redraws only when something moved.
fn install_load_poll(inner: &Rc<RefCell<Inner>>) {
    let poll = inner.clone();
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        let mut i = poll.borrow_mut();
        i.app.clock = i.now_ms();
        let active = i.app.workspaces.current().active;
        let Some((loading, progress)) = i.engine.load_state(active) else {
            return;
        };
        if i.app.record_progress(active, loading, progress) {
            i.render();
        }
    });
    unsafe {
        let t = NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.08, true, &block);
        std::mem::forget(t);
    }
    std::mem::forget(block);
}


/// Report memory conditions at startup.
///
/// Deliberately advisory, not a gate. The spike harness refuses to run on a
/// loaded machine because it allocates aggressively on purpose; a browser that
/// refused to open because the machine was busy would just be broken. Compact
/// adapts instead — [`LiveBudget::from_available`] sizes the live set to what
/// is actually free, and memory pressure shrinks it further.
pub fn preflight() -> Option<String> {
    match Guard::default().check() {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

#[allow(dead_code)]
fn unused(_: CGRect, _: NSEventType) {}

/// A minimal main menu.
///
/// Not decoration: an app with no menu bar does not participate properly in
/// activation, and standard bindings like ⌘Q and ⌘H simply do not work. The
/// browser's own commands live in the palette and the keymap, so this stays
/// deliberately small.
fn install_menu(mtm: MainThreadMarker, app: &NSApplication) {
    use objc2_app_kit::{NSMenu, NSMenuItem};

    let menubar = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    menubar.addItem(&app_item);

    let app_menu = NSMenu::new(mtm);
    let hide = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Hide Compact"),
            Some(objc2::sel!(hide:)),
            &NSString::from_str("h"),
        )
    };
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit Compact"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    app_menu.addItem(&hide);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&quit);
    app_item.setSubmenu(Some(&app_menu));

    app.setMainMenu(Some(&menubar));
}

/// What should receive the keyboard after the next render.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Address,
    Palette,
}
