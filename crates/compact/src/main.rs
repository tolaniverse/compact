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

        let mtm = objc2::MainThreadMarker::new()
            .expect("main() is the main thread");
        if let Err(e) = compact_ui::Shell::run(mtm) {
            eprintln!("compact: {e}");
            std::process::exit(1);
        }
    }
}
