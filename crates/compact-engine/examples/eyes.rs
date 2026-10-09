//! Drive an agent profile's eyes against a real page, headless.
//!
//! ```text
//! cargo run -p compact-engine --example eyes -- http://localhost:8765/ [out-dir]
//! ```
//!
//! Loads the page in an agent tab, then prints the snapshot and captured
//! console and network activity, and writes `page.png` plus `element.png`
//! (the snapshot's ref `e13`, the visible button) into `out-dir`. A second agent profile and your own
//! profile load `cookie.html` alongside, to show that cookies set in one are
//! invisible to the others.

#[cfg(target_os = "macos")]
fn main() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use compact_core::tab::TabId;
    use compact_core::workspace::Profile;
    use compact_engine::{TabEngine, WebKitEngine};
    use objc2::MainThreadMarker;
    use objc2_foundation::{NSDate, NSRunLoop};

    let mut args = std::env::args().skip(1);
    let base = args.next().unwrap_or_else(|| "http://localhost:8765/".into());
    let out = std::path::PathBuf::from(args.next().unwrap_or_else(|| ".".into()));

    let mtm = MainThreadMarker::new().expect("run on the main thread");
    let mut engine = WebKitEngine::headless(mtm);
    let spin = |secs: f64| {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(secs));
    };

    let page = TabId(1);
    let setter = TabId(2);
    let same_agent = TabId(3);
    let other_agent = TabId(4);
    let yours = TabId(5);
    let a = Profile::Agent("Claude Code\u{1f}~/site".into());
    let b = Profile::Agent("Muse\u{1f}~/site".into());
    engine.set_profile(page, a.clone());
    engine.set_profile(setter, a.clone());
    engine.set_profile(same_agent, a);
    engine.set_profile(other_agent, b);
    engine.set_profile(yours, Profile::Personal);

    engine.load(page, &base).unwrap();
    engine.load(setter, &format!("{base}cookie.html?set")).unwrap();
    spin(2.0);
    for t in [same_agent, other_agent, yours] {
        engine.load(t, &format!("{base}cookie.html")).unwrap();
    }
    spin(2.5);

    let results: Rc<RefCell<Vec<String>>> = Rc::default();

    let r = results.clone();
    engine.snapshot(page, move |s| r.borrow_mut().push(format!("== snapshot\n{}", s.unwrap_or_else(|e| format!("ERROR {e}")))));
    for (name, t) in [("same agent", same_agent), ("other agent", other_agent), ("yours", yours)] {
        let r = results.clone();
        engine.snapshot(t, move |s| {
            let s = s.unwrap_or_else(|e| format!("ERROR {e}"));
            let line = s.lines().find(|l| l.contains("cookie=")).unwrap_or("(no cookie line)").trim().to_string();
            r.borrow_mut().push(format!("== cookies seen by {name}: {line}"));
        });
    }
    for (file, target) in [("page.png", None), ("element.png", Some("e13")), ("hidden.png", Some("button")), ("missing.png", Some("#nope"))] {
        let r = results.clone();
        let path = out.join(file);
        engine.screenshot(page, target, move |png| {
            let msg = match png {
                Ok(bytes) => {
                    std::fs::write(&path, &bytes).unwrap();
                    format!("== {file}: {} bytes", bytes.len())
                }
                Err(e) => format!("== {file}: ERROR {e}"),
            };
            r.borrow_mut().push(msg);
        });
    }
    spin(2.0);
    // Capture lines arrive through the bridge's queue.
    for ev in compact_engine::drain_events() {
        if let compact_engine::EngineEvent::Capture { view, line } = ev {
            engine.ingest(view, &line);
        }
    }

    for r in results.borrow().iter() {
        println!("{r}");
    }
    match engine.capture(page) {
        Some(c) => {
            println!("== console");
            for e in c.console_since(0) {
                println!("  #{} {:<10} {} {}", e.seq, e.level.label(), e.text, e.source);
            }
            println!("== network");
            for e in c.network_since(0) {
                println!(
                    "  #{} {:<8} {} {} -> {} {}{}",
                    e.seq,
                    e.kind.label(),
                    e.method,
                    e.url,
                    e.status,
                    e.ms.map(|m| format!("{m}ms ")).unwrap_or_default(),
                    e.error
                );
            }
        }
        None => println!("== no capture for the agent tab"),
    }
    println!("== your tab has a capture buffer: {}", engine.capture(yours).is_some());
    for t in [page, setter, same_agent, other_agent, yours] {
        engine.destroy(t);
    }
    println!("== live views after destroy: {}", engine.live_count());
}

#[cfg(not(target_os = "macos"))]
fn main() {}
