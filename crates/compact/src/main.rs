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
        // Refuse to start on a machine that is already out of memory. Compact
        // spawns web content processes; starting into a thrashing system makes
        // the whole machine worse, not just this app.
        if let Err(e) = compact_ui::preflight() {
            eprintln!("compact: {e}");
            std::process::exit(1);
        }

        let mtm = objc2::MainThreadMarker::new()
            .expect("main() is the main thread");
        if let Err(e) = compact_ui::Shell::run(mtm) {
            eprintln!("compact: {e}");
            std::process::exit(1);
        }
    }
}
