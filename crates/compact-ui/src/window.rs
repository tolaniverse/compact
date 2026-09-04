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
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
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
use crate::widgets::{animated, rect, without_animation};

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
        let mut slide: Option<i8> = None;
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
                Effect::FocusAddressField => {
                    let f = self.chrome.address_field.clone();
                    unsafe {
                        self.window.makeFirstResponder(Some(&f));
                        let _: () = msg_send![&*f, selectText: std::ptr::null::<objc2::runtime::AnyObject>()];
                    }
                }
                Effect::FocusPaletteField => {
                    let f = self.overlays.palette_field.clone();
                    f.setStringValue(&NSString::from_str(""));
                    self.window.makeFirstResponder(Some(&f));
                }
                Effect::SlideWorkspace(dir) => slide = Some(dir),
            }
        }
        // Whatever just happened, the visible tab may have changed.
        let active = self.app.workspaces.current().active;
        match slide {
            Some(dir) if self.app.chrome.smooth_motion => self.slide_workspace(active, dir),
            _ => self.engine.show(active),
        }
    }

    /// The workspace transition: the outgoing page leaves in the direction of
    /// travel and the incoming one arrives from the opposite side.
    ///
    /// Both pages are real, mounted web views — no snapshot is taken. Snapshots
    /// would only help the incoming side, and the incoming side is exactly where
    /// they are unavailable: a hibernated tab has nothing rendered to capture,
    /// and keeping one per tab would mean caching a full-window image per tab,
    /// which costs orders of magnitude more than the 687 B session it is
    /// supposed to be accompanying.
    fn slide_workspace(&mut self, active: TabId, dir: i8) {
        use compact_core::theme::duration as dur;
        let outgoing = self.engine.visible();
        if outgoing == Some(active) || outgoing.is_none() {
            self.engine.show(active);
            return;
        }
        let (w, _) = self.size();
        let travel = w * 0.15;
        let d = dir as f64;

        // Mount the incoming page beside the outgoing one, pre-offset to the
        // side it should arrive from.
        self.engine.mount_alongside(active);
        let Some(incoming) = self.engine.view(active).map(host_layer_of) else {
            self.engine.show(active);
            return;
        };
        without_animation(|| {
            crate::widgets::set_translate_x(incoming, d * travel);
            crate::widgets::set_host_opacity(incoming, 0.0);
        });

        // Push the outgoing page out under it.
        if let Some(prev) = outgoing.and_then(|id| self.engine.view(id)).map(host_layer_of) {
            animated(dur::SLIDE_OUT, || {
                crate::widgets::set_translate_x(prev, -d * travel);
                crate::widgets::set_host_opacity(prev, 0.0);
            });
        }
        animated(dur::SLIDE_IN, || {
            crate::widgets::set_translate_x(incoming, 0.0);
            crate::widgets::set_host_opacity(incoming, 1.0);
        });

        // Detach the outgoing page once it is off screen, and reset the
        // transform so it is centred again the next time it is shown.
        if let Some(prev) = outgoing {
            let me = self.self_ref.clone();
            queue_after(dur::SLIDE_IN, move || {
                if let Some(inner) = me.upgrade() {
                    let i = inner.borrow_mut();
                    if let Some(l) = i.engine.view(prev).map(host_layer_of) {
                        without_animation(|| {
                            crate::widgets::set_translate_x(l, 0.0);
                            crate::widgets::set_host_opacity(l, 1.0);
                        });
                    }
                    i.engine.unmount(prev);
                }
            });
        }
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
        // the window regardless of what is revealed.
        if let Some(v) = self.engine.view(self.app.workspaces.current().active) {
            v.setFrame(rect(0.0, 0.0, size.0, size.1));
        }
    }

    fn dispatch(&mut self, action: Action) {
        let fx = self.app.apply(action);
        self.perform(fx);
        self.render();
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
            i.perform(fx);
            i.render();
        }

        install_monitors(&inner);
        install_ticker(&inner);

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
        let editing = i.app.overlay == Overlay::Palette
            || i.window.firstResponder()
                .map(|r| {
                    let f: *const _ = &*i.chrome.address_field;
                    std::ptr::eq(&*r as *const _ as *const u8, f as *const u8)
                })
                .unwrap_or(false);

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
                    i.perform(fx);
                    i.render();
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
            let v = i.chrome.address_field.stringValue().to_string();
            i.app.set_address_draft(v);
            let fx = i.app.commit_address();
            i.perform(fx);
            i.window.makeFirstResponder(None);
            i.render();
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
                        i.perform(fx);
                        i.render();
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

        let hit = i.chrome.hit(&i.app, x, y);
        match hit {
            Some(Hit::SelectTab(id)) => i.dispatch(Action::SelectTab(id)),
            Some(Hit::CloseTab(id)) => i.dispatch(Action::CloseTab(id)),
            Some(Hit::NewTab) => i.dispatch(Action::NewTab),
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
        let mut dirty = i.app.notifications.expire(now);

        // Adopt real page titles as navigations settle.
        let ids: Vec<_> = i.app.workspaces.current().tabs.iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(title) = i.engine.title(id) {
                let cur = i
                    .app
                    .workspaces
                    .locate(id)
                    .map(|(_, t)| t.title.clone())
                    .unwrap_or_default();
                if cur != title {
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
                }
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

/// Run `f` on the main queue after `delay` seconds.
fn queue_after(delay: f64, f: impl FnOnce() + 'static) {
    let cell = RefCell::new(Some(f));
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        if let Some(f) = cell.borrow_mut().take() {
            f();
        }
    });
    unsafe {
        let t = NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay.max(0.001), false, &block);
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

/// A view's backing layer as the untyped pointer AppKit hands back.
pub fn host_layer_of<T: objc2::Message>(view: &T) -> *mut objc2::runtime::AnyObject {
    unsafe { objc2::msg_send![view, layer] }
}
