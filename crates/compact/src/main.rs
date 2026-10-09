//! Compact — a keyboard-first, low-RAM browser.

fn main() {
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("Compact currently has a macOS backend only.");
        eprintln!("The engine seam (compact-engine::TabEngine) is where Linux and Windows plug in.");
        std::process::exit(1);
    }

    #[cfg(target_os = "macos")]
    {
        // Advisory only. Compact adapts to a loaded machine by sizing its live
        // tab set to what is free, so a tight machine means fewer live tabs,
        // not a refusal to open.
        if let Some(warning) = compact_ui::preflight() {
            eprintln!("compact: running with a reduced live-tab budget — {warning}");
        }

        // `--demo` opens on the canvas's sample workspaces and saves nothing,
        // so screenshots never touch — or leak — a real profile.
        let demo = std::env::args().skip(1).any(|a| a == "--demo");
        let (app, store) = if demo {
            (compact_core::App::new(), None)
        } else {
            match compact_ui::Store::default_dir() {
                Some(dir) => {
                    let opened = compact_ui::Store::open(&dir);
                    if let Some(w) = opened.warning {
                        eprintln!("compact: {w}");
                    }
                    (opened.app, Some(opened.store))
                }
                None => {
                    eprintln!("compact: no home directory; this session will not be saved");
                    (compact_core::App::fresh(), None)
                }
            }
        };

        let mtm = objc2::MainThreadMarker::new()
            .expect("main() is the main thread");
        if let Err(e) = compact_ui::Shell::run(mtm, app, store) {
            eprintln!("compact: {e}");
            std::process::exit(1);
        }
    }
}
