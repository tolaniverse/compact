//! Modal surfaces: command palette, workspace switcher, shortcuts, settings.
//!
//! All four are built once and shown by frame and visibility rather than being
//! constructed on demand — they are small, and rebuilding a layer tree at the
//! moment the user hits ⌘K is exactly when the allocation would be visible.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAnimatablePropertyContainer, NSTextField, NSView};

use compact_core::command::shortcuts;
use compact_core::theme::{accent, bg, border, fg, fill, metric, radius, Rgba};
use compact_core::{chrome::TabLayout, App, Overlay};

use crate::widgets::*;

struct Row {
    root: Layer,
    icon: Layer,
    label: Layer,
    hint: Layer,
}

impl Row {
    fn new(parent: &Layer) -> Self {
        let root = panel(rect(0.0, 0.0, 0.0, 0.0), Rgba::white(0.0), 8.0, None);
        let icon = text("", 11.0, fg::DIM, false, Align::Center);
        let label = text("", 13.0, fg::SECONDARY, false, Align::Left);
        let hint = text("", 10.0, fg::DIM, true, Align::Right);
        for l in [&icon, &label, &hint] {
            add_sublayer(&root, l);
        }
        add_sublayer(parent, &root);
        Self {
            root,
            icon,
            label,
            hint,
        }
    }
}

struct Card {
    root: Layer,
    dot: Layer,
    name: Layer,
    key: Layer,
    badge: Layer,
    badge_text: Layer,
    count: Layer,
    previews: [Layer; 3],
}

impl Card {
    fn new(parent: &Layer) -> Self {
        let root = panel(
            rect(0.0, 0.0, metric::WS_CARD_WIDTH, metric::WS_CARD_HEIGHT),
            Rgba::new(18.0 / 255.0, 18.0 / 255.0, 23.0 / 255.0, 0.9),
            radius::CARD,
            Some((border::CARD, 1.0)),
        );
        let dot = panel(rect(16.0, 20.0, 7.0, 7.0), accent::PRIMARY, 3.5, None);
        let name = text("", 13.0, fg::PRIMARY, false, Align::Left);
        let key = text("", 10.0, fg::DIM, true, Align::Right);
        let badge = panel(rect(0.0, 0.0, 0.0, 0.0), accent::PRIMARY, 9.5, None);
        let badge_text = text("", 10.0, bg::WINDOW, true, Align::Center);
        let count = text("", 10.0, fg::FAINT, true, Align::Left);
        let previews = [
            panel(rect(16.0, 96.0, 0.0, 7.0), Rgba::white(0.07), 3.5, None),
            panel(rect(16.0, 109.0, 0.0, 7.0), Rgba::white(0.07), 3.5, None),
            panel(rect(16.0, 122.0, 0.0, 7.0), Rgba::white(0.07), 3.5, None),
        ];
        for l in [&dot, &name, &key, &badge, &count] {
            add_sublayer(&root, l);
        }
        add_sublayer(&badge, &badge_text);
        for p in &previews {
            add_sublayer(&root, p);
        }
        add_sublayer(parent, &root);
        Self {
            root,
            dot,
            name,
            key,
            badge,
            badge_text,
            count,
            previews,
        }
    }
}

pub struct Overlays {
    size: (f64, f64),
    /// Last drawer state pushed, so the slide animates on the transition only.
    drawer_open: Option<bool>,
    /// Whether the user wants motion at all.
    pub motion: bool,

    // Command palette.
    pub palette: Retained<NSView>,
    palette_root: Layer,
    palette_panel: Layer,
    palette_caret: Layer,
    palette_esc: Layer,
    pub palette_field: Retained<NSTextField>,
    palette_rows: Vec<Row>,

    // Workspace switcher.
    pub switcher: Retained<NSView>,
    switcher_root: Layer,
    switcher_label: Layer,
    cards: Vec<Card>,

    // Shortcuts.
    pub keymap: Retained<NSView>,
    keymap_root: Layer,
    keymap_panel: Layer,
    keymap_title: Layer,
    keymap_sub: Layer,
    keymap_rows: Vec<Row>,

    // Settings drawer.
    pub settings: Retained<NSView>,
    settings_root: Layer,
    settings_edge: Layer,
    settings_title: Layer,
    settings_close: Layer,
    layout_head: Layer,
    seg_track: Layer,
    seg_vert: Layer,
    seg_vert_label: Layer,
    seg_horiz: Layer,
    seg_horiz_label: Layer,
    reveal_head: Layer,
    reveal_value: Layer,
    reveal_track: Layer,
    reveal_fill: Layer,
    reveal_knob: Layer,
    reveal_note: Layer,
    toggles: Vec<(Layer, Layer, Layer, Layer)>,
    version_label: Layer,
    ram_label: Layer,
}

/// A full-window scrim behind a modal surface.
///
/// Backed by `NSVisualEffectView` rather than a plain tinted layer. The canvas
/// asks for `backdrop-filter: blur()` on every scrim, and it is also the only
/// thing that works: a plain `CALayer` with alpha composites weakly over a
/// `WKWebView` (which renders through a remote layer context), leaving page text
/// legible straight through a 96%-opaque panel.
fn scrim_host(mtm: MainThreadMarker, tint: Rgba) -> (Retained<NSView>, Layer) {
    let v = blur_view(mtm, rect(0.0, 0.0, 0.0, 0.0), 0.0);
    let root = layer();
    unsafe {
        use objc2::msg_send;
        let _: () = msg_send![&*root, setGeometryFlipped: true];
        let tint = cg_color(tint);
        let _: () = msg_send![&*root, setBackgroundColor: &*tint];
        v.setWantsLayer(true);
        let hostl: *mut objc2::runtime::AnyObject = msg_send![&*v, layer];
        let _: () = msg_send![hostl, addSublayer: &*root];
    }
    (Retained::into_super(v), root)
}

impl Overlays {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let zero = rect(0.0, 0.0, 0.0, 0.0);

        // ---- palette ----
        let (palette, palette_root) = scrim_host(mtm, bg::SCRIM);
        let palette_panel = panel(
            zero,
            bg::PALETTE,
            radius::PALETTE,
            Some((border::RAISED, 1.0)),
        );
        add_sublayer(&palette_root, &palette_panel);
        let palette_caret = text("›", 12.0, fg::DIM, false, Align::Center);
        let palette_esc = text("ESC", 10.0, fg::FAINT, true, Align::Right);
        add_sublayer(&palette_panel, &palette_caret);
        add_sublayer(&palette_panel, &palette_esc);
        let palette_field = field(mtm, zero, 14.0, false, fg::PRIMARY);
        palette_field.setPlaceholderString(Some(&objc2_foundation::NSString::from_str(
            "Search commands, tabs, history…",
        )));
        palette.addSubview(&palette_field);

        // ---- switcher ----
        let (switcher, switcher_root) = scrim_host(mtm, bg::SCRIM_HEAVY);
        let switcher_label = text(
            "WORKSPACES · ⌘1 – ⌘3",
            10.5,
            Rgba::hex(0x63636D),
            true,
            Align::Center,
        );
        add_sublayer(&switcher_root, &switcher_label);

        // ---- shortcuts ----
        let (keymap, keymap_root) =
            scrim_host(mtm, Rgba::new(5.0 / 255.0, 5.0 / 255.0, 7.0 / 255.0, 0.70));
        let keymap_panel = panel(
            zero,
            Rgba::new(20.0 / 255.0, 20.0 / 255.0, 25.0 / 255.0, 0.96),
            radius::PALETTE,
            Some((border::RAISED, 1.0)),
        );
        add_sublayer(&keymap_root, &keymap_panel);
        let keymap_title = text("Keyboard", 15.0, fg::PRIMARY, false, Align::Left);
        let keymap_sub = text(
            "everything is remappable",
            10.0,
            fg::DIM,
            true,
            Align::Right,
        );
        add_sublayer(&keymap_panel, &keymap_title);
        add_sublayer(&keymap_panel, &keymap_sub);

        // ---- settings ----
        // A blurred backdrop, like the rest of the chrome. The canvas asks for
        // `blur(26px)` under a 97% tint; without the blur, the tint alone lets
        // page text read through and the panel looks broken.
        let settings_blur = blur_view(mtm, zero, 0.0);
        let settings_root = layer();
        unsafe {
            use objc2::msg_send;
            let _: () = msg_send![&*settings_root, setGeometryFlipped: true];
            let sc = cg_color(bg::SETTINGS);
            let _: () = msg_send![&*settings_root, setBackgroundColor: &*sc];
            settings_blur.setWantsLayer(true);
            let hostl: *mut objc2::runtime::AnyObject = msg_send![&*settings_blur, layer];
            let _: () = msg_send![hostl, addSublayer: &*settings_root];
        }
        // Left hairline, matching `border-left: rgba(255,255,255,.09)`.
        let settings_edge = panel(zero, Rgba::white(0.09), 0.0, None);
        add_sublayer(&settings_root, &settings_edge);
        let settings = Retained::into_super(settings_blur);

        let settings_title = text("Settings", 15.0, fg::PRIMARY, false, Align::Left);
        let settings_close = text("CLOSE", 10.0, fg::DIM, true, Align::Right);
        let layout_head = text("TAB LAYOUT", 11.0, Rgba::hex(0x63636D), false, Align::Left);
        let seg_track = panel(zero, Rgba::white(0.04), 9.0, Some((border::SUBTLE, 1.0)));
        let seg_vert = panel(zero, Rgba::white(0.10), 7.0, None);
        let seg_vert_label = text("Vertical", 12.0, fg::PRIMARY, false, Align::Center);
        let seg_horiz = panel(zero, Rgba::white(0.0), 7.0, None);
        let seg_horiz_label = text(
            "Horizontal",
            12.0,
            Rgba::hex(0x8A8A94),
            false,
            Align::Center,
        );
        let reveal_head = text(
            "REVEAL DISTANCE",
            11.0,
            Rgba::hex(0x63636D),
            false,
            Align::Left,
        );
        let reveal_value = text("", 10.5, Rgba::hex(0x8A8A94), true, Align::Right);
        let reveal_track = panel(zero, Rgba::white(0.12), 2.0, None);
        let reveal_fill = panel(zero, accent::PRIMARY, 2.0, None);
        let reveal_knob = panel(zero, Rgba::hex(0xFFFFFF), 7.0, None);
        let reveal_note = text(
            "How close the cursor gets before chrome tears in.",
            11.5,
            Rgba::hex(0x63636D),
            false,
            Align::Left,
        );
        let version_label = text("COMPACT 0.1", 10.0, fg::FAINT, true, Align::Left);
        let ram_label = text("", 10.0, fg::FAINT, true, Align::Right);
        for l in [
            &settings_title,
            &settings_close,
            &layout_head,
            &seg_track,
            &seg_vert,
            &seg_horiz,
            &reveal_head,
            &reveal_value,
            &reveal_track,
            &reveal_fill,
            &reveal_knob,
            &reveal_note,
            &version_label,
            &ram_label,
        ] {
            add_sublayer(&settings_root, l);
        }
        add_sublayer(&seg_vert, &seg_vert_label);
        add_sublayer(&seg_horiz, &seg_horiz_label);

        let mut toggles = Vec::new();
        for label in [
            "Pin chrome open",
            "Smooth motion",
            "Dim page while chrome is out",
        ] {
            let row = panel(zero, Rgba::white(0.0), 0.0, None);
            let lbl = text(label, 12.5, Rgba::hex(0xC6C6CE), false, Align::Left);
            let track = panel(zero, fill::TRACK_OFF, 10.0, None);
            let knob = panel(zero, Rgba::hex(0xFFFFFF), 8.0, None);
            add_sublayer(&row, &lbl);
            add_sublayer(&row, &track);
            add_sublayer(&track, &knob);
            add_sublayer(&settings_root, &row);
            toggles.push((row, lbl, track, knob));
        }

        Self {
            size: (0.0, 0.0),
            drawer_open: None,
            motion: true,
            palette,
            palette_root,
            palette_panel,
            palette_caret,
            palette_esc,
            palette_field,
            palette_rows: Vec::new(),
            switcher,
            switcher_root,
            switcher_label,
            cards: Vec::new(),
            keymap,
            keymap_root,
            keymap_panel,
            keymap_title,
            keymap_sub,
            keymap_rows: Vec::new(),
            settings,
            settings_root,
            settings_edge,
            settings_title,
            settings_close,
            layout_head,
            seg_track,
            seg_vert,
            seg_vert_label,
            seg_horiz,
            seg_horiz_label,
            reveal_head,
            reveal_value,
            reveal_track,
            reveal_fill,
            reveal_knob,
            reveal_note,
            toggles,
            version_label,
            ram_label,
        }
    }

    pub fn views(&self) -> Vec<Retained<NSView>> {
        vec![
            self.settings.clone(),
            self.palette.clone(),
            self.switcher.clone(),
            self.keymap.clone(),
        ]
    }

    pub fn set_size(&mut self, size: (f64, f64)) {
        self.size = size;
    }

    pub fn sync(&mut self, app: &App) {
        let (w, h) = self.size;
        without_animation(|| {
            self.sync_palette(app, w, h);
            self.sync_switcher(app, w, h);
            self.sync_keymap(app, w, h);
        });
        // Outside the suppression block: the drawer slide and the toggle knobs
        // are transitions the design asks for, not state sync.
        self.motion = app.chrome.smooth_motion;
        self.sync_settings(app, w, h);
    }

    fn sync_palette(&mut self, app: &App, w: f64, h: f64) {
        let show = app.overlay == Overlay::Palette;
        self.palette.setHidden(!show);
        if !show {
            return;
        }
        self.palette.setFrame(rect(0.0, 0.0, w, h));
        set_frame(&self.palette_root, rect(0.0, 0.0, w, h));

        let items = app.commands();
        let pw = (w * 0.86).min(metric::PALETTE_WIDTH);
        let header = 52.0;
        // Sized to content. The canvas caps the list and scrolls it, but the
        // command set is nine items and never filters longer, so a scroll view
        // here would be machinery that can only ever be a no-op — and clamping
        // without one just clips the last row.
        let list_h = items.len() as f64 * metric::PALETTE_ROW_HEIGHT + 14.0;
        let ph = header + list_h;
        let px = (w - pw) / 2.0;
        let py = h * 0.12;
        set_frame(&self.palette_panel, rect(px, py, pw, ph));
        set_frame(&self.palette_caret, rect(15.0, 19.0, 12.0, 15.0));
        set_frame(&self.palette_esc, rect(pw - 50.0, 20.0, 35.0, 13.0));
        self.palette_field
            .setFrame(rect(px + 37.0, h - py - header + 16.0, pw - 100.0, 20.0));

        while self.palette_rows.len() < items.len() {
            let r = Row::new(&self.palette_panel);
            self.palette_rows.push(r);
        }
        for (i, row) in self.palette_rows.iter().enumerate() {
            match items.get(i) {
                None => set_hidden(&row.root, true),
                Some(c) => {
                    let on = i == app.selection;
                    let ry = header + 7.0 + i as f64 * metric::PALETTE_ROW_HEIGHT;
                    set_hidden(&row.root, false);
                    set_frame(
                        &row.root,
                        rect(7.0, ry, pw - 14.0, metric::PALETTE_ROW_HEIGHT),
                    );
                    set_fill(&row.root, if on { fill::HOVER } else { Rgba::white(0.0) });
                    set_frame(&row.icon, rect(11.0, 12.0, 16.0, 14.0));
                    set_text(&row.icon, c.icon);
                    set_color(&row.icon, if on { accent::PRIMARY } else { fg::DIM });
                    set_frame(&row.label, rect(38.0, 11.0, pw - 140.0, 16.0));
                    set_text(&row.label, &c.label);
                    set_color(
                        &row.label,
                        if on { fg::PRIMARY } else { Rgba::hex(0xB4B4BE) },
                    );
                    set_frame(&row.hint, rect(pw - 90.0, 12.0, 68.0, 13.0));
                    set_text(&row.hint, c.hint);
                }
            }
        }
    }

    fn sync_switcher(&mut self, app: &App, w: f64, h: f64) {
        let show = app.overlay == Overlay::Switcher;
        self.switcher.setHidden(!show);
        if !show {
            return;
        }
        self.switcher.setFrame(rect(0.0, 0.0, w, h));
        set_frame(&self.switcher_root, rect(0.0, 0.0, w, h));

        let n = app.workspaces.spaces.len();
        let cw = metric::WS_CARD_WIDTH;
        let ch = metric::WS_CARD_HEIGHT;
        let gap = 26.0;
        let total = n as f64 * cw + (n as f64 - 1.0) * gap;
        let x0 = (w - total) / 2.0;
        let cy = (h - ch) / 2.0;
        set_frame(&self.switcher_label, rect(0.0, cy - 60.0, w, 14.0));

        while self.cards.len() < n {
            let c = Card::new(&self.switcher_root);
            self.cards.push(c);
        }
        // The canvas tilts inactive cards; the active one sits flat and lifted.
        let tilts = [-3.5f64, 1.5, 4.0];
        let lifts = [6.0f64, -4.0, 8.0];
        for (i, card) in self.cards.iter().enumerate() {
            match app.workspaces.spaces.get(i) {
                None => set_hidden(&card.root, true),
                Some(ws) => {
                    let on = i == app.workspaces.current;
                    let x = x0 + i as f64 * (cw + gap);
                    let y = cy + if on { -10.0 } else { lifts[i % 3] };
                    set_hidden(&card.root, false);
                    set_frame(&card.root, rect(x, y, cw, ch));
                    set_border(
                        &card.root,
                        if on {
                            border::CARD_ACTIVE
                        } else {
                            border::CARD
                        },
                        1.0,
                    );
                    rotate(&card.root, if on { 0.0 } else { tilts[i % 3].to_radians() });
                    set_fill(&card.dot, ws.dot);
                    set_frame(&card.name, rect(31.0, 16.0, cw - 110.0, 17.0));
                    set_text(&card.name, &ws.name);
                    set_color(
                        &card.name,
                        if on { fg::PRIMARY } else { Rgba::hex(0xA6A6B0) },
                    );
                    set_frame(&card.key, rect(cw - 46.0, 17.0, 30.0, 14.0));
                    set_text(&card.key, &format!("⌘{}", i + 1));

                    let has_unread = ws.unread > 0;
                    set_hidden(&card.badge, !has_unread);
                    if has_unread {
                        set_frame(&card.badge, rect(cw - 80.0, 14.0, 26.0, 19.0));
                        set_fill(&card.badge, ws.dot);
                        set_frame(&card.badge_text, rect(0.0, 3.0, 26.0, 13.0));
                        set_text(&card.badge_text, &ws.unread.to_string());
                    }

                    // Preview bars stand in for page content; deliberately not
                    // real thumbnails. A cached 2x preview per tab would cost
                    // ~240 KB each — a hundred times a hibernated tab.
                    for (j, p) in card.previews.iter().enumerate() {
                        let widths = [0.86, 0.64, 0.72];
                        set_frame(
                            p,
                            rect(16.0, 96.0 + j as f64 * 13.0, (cw - 32.0) * widths[j], 7.0),
                        );
                    }
                    set_frame(&card.count, rect(16.0, ch - 26.0, 120.0, 13.0));
                    set_text(&card.count, &ws.count_label());
                }
            }
        }
    }

    fn sync_keymap(&mut self, app: &App, w: f64, h: f64) {
        let show = app.overlay == Overlay::Shortcuts;
        self.keymap.setHidden(!show);
        if !show {
            return;
        }
        self.keymap.setFrame(rect(0.0, 0.0, w, h));
        set_frame(&self.keymap_root, rect(0.0, 0.0, w, h));

        let pw = (w * 0.88).min(660.0);
        // Read from the live keymap: bindings are editable, and the overlay
        // whose job is telling you which key does what must not show defaults.
        let keys = shortcuts(&app.settings.keymap);
        let rows = keys.len().div_ceil(2);
        let ph = 26.0 + 40.0 + rows as f64 * 33.0 + 26.0;
        let px = (w - pw) / 2.0;
        let py = (h - ph) / 2.0;
        set_frame(&self.keymap_panel, rect(px, py, pw, ph));
        set_frame(&self.keymap_title, rect(28.0, 24.0, 200.0, 20.0));
        set_frame(&self.keymap_sub, rect(pw - 240.0, 27.0, 212.0, 14.0));

        while self.keymap_rows.len() < keys.len() {
            let r = Row::new(&self.keymap_panel);
            self.keymap_rows.push(r);
        }
        let col_w = (pw - 56.0 - 34.0) / 2.0;
        for (i, row) in self.keymap_rows.iter().enumerate() {
            match keys.get(i) {
                None => set_hidden(&row.root, true),
                Some(s) => {
                    // Two columns, filling left-to-right like the CSS grid.
                    let col = i % 2;
                    let line = i / 2;
                    let x = 28.0 + col as f64 * (col_w + 34.0);
                    let y = 66.0 + line as f64 * 33.0;
                    set_hidden(&row.root, false);
                    set_frame(&row.root, rect(x, y, col_w, 33.0));
                    set_hidden(&row.icon, true);
                    set_frame(&row.label, rect(0.0, 8.0, col_w - 90.0, 16.0));
                    set_text(&row.label, s.label);
                    set_color(&row.label, Rgba::hex(0xB9B9C2));
                    set_frame(&row.hint, rect(col_w - 84.0, 9.0, 80.0, 14.0));
                    set_text(&row.hint, &s.keys);
                    set_color(&row.hint, Rgba::hex(0x787882));
                }
            }
        }
    }

    fn sync_settings(&mut self, app: &App, w: f64, h: f64) {
        let open = app.overlay == Overlay::Settings;
        let sw = metric::SETTINGS_WIDTH;
        // Slides in from the right rather than being hidden outright, so the
        // transition has somewhere to come from. `translateX(101%)` in the
        // canvas — just past the edge, so the shadow clears too.
        let x = if open { w - sw } else { w + 4.0 };
        let changed = self.drawer_open != Some(open);
        self.drawer_open = Some(open);

        // Stay mounted while closed: a `setHidden` view cannot animate out.
        self.settings.setHidden(false);
        if changed && self.motion {
            animate(compact_core::theme::duration::DRAWER, || {
                self.settings.animator().setFrame(rect(x, 0.0, sw, h))
            });
        } else {
            without_animation(|| self.settings.setFrame(rect(x, 0.0, sw, h)));
        }
        set_frame(&self.settings_root, rect(0.0, 0.0, sw, h));
        if !open {
            return;
        }
        set_frame(&self.settings_edge, rect(0.0, 0.0, 1.0, h));
        let pad = 24.0;
        set_frame(&self.settings_title, rect(pad, 26.0, 200.0, 20.0));
        set_frame(
            &self.settings_close,
            rect(sw - pad - 60.0, 30.0, 60.0, 14.0),
        );

        set_frame(&self.layout_head, rect(pad, 78.0, 200.0, 14.0));
        let seg_y = 98.0;
        let seg_w = sw - pad * 2.0;
        set_frame(&self.seg_track, rect(pad, seg_y, seg_w, 37.0));
        let half = (seg_w - 6.0 - 6.0) / 2.0;
        let vertical = app.chrome.layout == TabLayout::Vertical;
        set_frame(&self.seg_vert, rect(pad + 3.0, seg_y + 3.0, half, 31.0));
        set_frame(&self.seg_vert_label, rect(0.0, 8.0, half, 16.0));
        set_fill(
            &self.seg_vert,
            if vertical {
                Rgba::white(0.10)
            } else {
                Rgba::white(0.0)
            },
        );
        set_color(
            &self.seg_vert_label,
            if vertical {
                fg::PRIMARY
            } else {
                Rgba::hex(0x8A8A94)
            },
        );
        set_frame(
            &self.seg_horiz,
            rect(pad + 9.0 + half, seg_y + 3.0, half, 31.0),
        );
        set_frame(&self.seg_horiz_label, rect(0.0, 8.0, half, 16.0));
        set_fill(
            &self.seg_horiz,
            if vertical {
                Rgba::white(0.0)
            } else {
                Rgba::white(0.10)
            },
        );
        set_color(
            &self.seg_horiz_label,
            if vertical {
                Rgba::hex(0x8A8A94)
            } else {
                fg::PRIMARY
            },
        );

        let rv_y = 168.0;
        set_frame(&self.reveal_head, rect(pad, rv_y, 200.0, 14.0));
        set_frame(&self.reveal_value, rect(sw - pad - 80.0, rv_y, 80.0, 14.0));
        let track_y = rv_y + 26.0;
        let track_w = sw - pad * 2.0;
        set_frame(&self.reveal_track, rect(pad, track_y, track_w, 4.0));
        let t = (app.chrome.threshold - compact_core::chrome::REVEAL_THRESHOLD_MIN)
            / (compact_core::chrome::REVEAL_THRESHOLD_MAX
                - compact_core::chrome::REVEAL_THRESHOLD_MIN);
        set_frame(&self.reveal_fill, rect(pad, track_y, track_w * t, 4.0));
        set_frame(
            &self.reveal_knob,
            rect(pad + track_w * t - 7.0, track_y - 5.0, 14.0, 14.0),
        );
        set_text(&self.reveal_value, &app.chrome.threshold_label());
        set_frame(&self.reveal_note, rect(pad, track_y + 16.0, track_w, 32.0));

        let vals = [
            app.chrome.pinned,
            app.chrome.smooth_motion,
            app.chrome.dim_page,
        ];
        let ty0 = track_y + 62.0;
        for (i, (row, lbl, track, knob)) in self.toggles.iter().enumerate() {
            let y = ty0 + i as f64 * 44.0;
            set_frame(row, rect(pad, y, sw - pad * 2.0, 44.0));
            set_frame(lbl, rect(0.0, 14.0, sw - pad * 2.0 - 50.0, 16.0));
            let on = vals[i];
            set_frame(track, rect(sw - pad * 2.0 - 36.0, 12.0, 36.0, 20.0));
            // The knob travels and the track tints on the same short curve, so
            // the switch reads as one movement rather than two events.
            animated(compact_core::theme::duration::TOGGLE, || {
                set_fill(track, if on { accent::PRIMARY } else { fill::TRACK_OFF });
                set_frame(knob, rect(if on { 18.0 } else { 2.0 }, 2.0, 16.0, 16.0));
            });
        }

        set_frame(&self.version_label, rect(pad, h - 34.0, 140.0, 13.0));
        set_frame(
            &self.ram_label,
            rect(sw - pad - 140.0, h - 34.0, 140.0, 13.0),
        );
        set_text(&self.ram_label, &app.memory_label());
    }

    /// Hit-test the settings drawer, which is the only overlay with controls
    /// that are not a simple list.
    pub fn settings_hit(&self, _app: &App, x: f64, y: f64) -> Option<SettingsHit> {
        let (w, h) = self.size;
        let sw = metric::SETTINGS_WIDTH;
        let lx = x - (w - sw);
        if lx < 0.0 {
            return None;
        }
        let pad = 24.0;
        if (26.0..50.0).contains(&y) && lx > sw - pad - 60.0 {
            return Some(SettingsHit::Close);
        }
        if (98.0..135.0).contains(&y) {
            let half = (sw - pad * 2.0) / 2.0;
            return Some(if lx < pad + half {
                SettingsHit::Layout(TabLayout::Vertical)
            } else {
                SettingsHit::Layout(TabLayout::Horizontal)
            });
        }
        let track_y = 194.0;
        if y >= track_y - 12.0 && y < track_y + 16.0 {
            let track_w = sw - pad * 2.0;
            let t = ((lx - pad) / track_w).clamp(0.0, 1.0);
            let range = compact_core::chrome::REVEAL_THRESHOLD_MAX
                - compact_core::chrome::REVEAL_THRESHOLD_MIN;
            let raw = compact_core::chrome::REVEAL_THRESHOLD_MIN + t * range;
            let step = compact_core::chrome::REVEAL_THRESHOLD_STEP;
            return Some(SettingsHit::Threshold((raw / step).round() * step));
        }
        let ty0 = track_y + 62.0;
        for i in 0..3 {
            let ry = ty0 + i as f64 * 44.0;
            if y >= ry && y < ry + 44.0 {
                return Some(SettingsHit::Toggle(i));
            }
        }
        let _ = h;
        Some(SettingsHit::Chrome)
    }

    /// Which workspace card, if any, a switcher click landed on.
    pub fn switcher_hit(&self, app: &App, x: f64, y: f64) -> Option<usize> {
        let (w, h) = self.size;
        let n = app.workspaces.spaces.len();
        let cw = metric::WS_CARD_WIDTH;
        let ch = metric::WS_CARD_HEIGHT;
        let gap = 26.0;
        let total = n as f64 * cw + (n as f64 - 1.0) * gap;
        let x0 = (w - total) / 2.0;
        let cy = (h - ch) / 2.0;
        // Generous vertical band: the cards are tilted and lifted, so testing
        // the untransformed rect would miss their corners.
        if y < cy - 20.0 || y > cy + ch + 20.0 {
            return None;
        }
        for i in 0..n {
            let cx = x0 + i as f64 * (cw + gap);
            if x >= cx && x < cx + cw {
                return Some(i);
            }
        }
        None
    }

    /// Which palette row a click landed on.
    pub fn palette_hit(&self, app: &App, x: f64, y: f64) -> Option<usize> {
        let (w, h) = self.size;
        let items = app.commands();
        let pw = (w * 0.86).min(metric::PALETTE_WIDTH);
        let px = (w - pw) / 2.0;
        let py = h * 0.12;
        if x < px || x > px + pw {
            return None;
        }
        let list_top = py + 52.0 + 7.0;
        if y < list_top {
            return None;
        }
        let idx = ((y - list_top) / metric::PALETTE_ROW_HEIGHT) as usize;
        (idx < items.len()).then_some(idx)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingsHit {
    Close,
    Layout(TabLayout),
    Threshold(f64),
    Toggle(usize),
    Chrome,
}

/// Rotate a layer about its centre.
fn rotate(l: &Layer, radians: f64) {
    unsafe {
        use objc2::msg_send;
        let cls = objc2::runtime::AnyClass::get(c"NSValue").expect("NSValue");
        // CATransform3DMakeRotation about the z axis.
        let t = ca_rotation_z(radians);
        let v: *mut objc2::runtime::AnyObject = msg_send![cls, valueWithBytes: &t as *const _ as *const std::ffi::c_void,
                           objCType: c"{CATransform3D=dddddddddddddddd}".as_ptr()];
        let _: () = msg_send![&**l, setValue: v, forKeyPath: &*objc2_foundation::NSString::from_str("transform")];
    }
}

/// Column-major 4x4 rotation about z, matching `CATransform3D`.
fn ca_rotation_z(a: f64) -> [f64; 16] {
    let (s, c) = a.sin_cos();
    [
        c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}
