//! The guarded memory harness.
//!
//! Answers one question: **does Compact's two-tier tab model actually hold its
//! memory claim when driven through the real engine?**
//!
//! Two things make this different from a standalone spike. It drives
//! `compact-engine::TabEngine` — the same code path the browser uses — so the
//! result describes production behaviour rather than a parallel mock. And it
//! refuses to run on a machine that cannot afford it: an unguarded 30-webview
//! run wedged this laptop on 2026-09-03, and "the test crashed the host" is not
//! a measurement.
//!
//! Every rule from M0 is enforced here rather than left to the operator:
//! `phys_footprint` not RSS, pid-set difference not parentage, and a sampling
//! schedule that outlasts WebKit's ~5 s process reuse cache.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use objc2::MainThreadMarker;
use objc2_foundation::{NSDate, NSRunLoop};

use compact_core::policy::Pressure;
use compact_core::tab::TabId;
use compact_engine::{TabEngine, WebKitEngine};
use compact_mem::{human, sample, Guard, PressureMonitor, WebKitCensus, RECLAIM_SCHEDULE_SECS};

/// The M0 gate: a hibernated tab must cost less than this.
const HIBERNATION_GATE: usize = 50 * 1024;

struct Args {
    tabs: usize,
    live: usize,
    url: String,
    /// Hold every tab live at once. This is the shape that wedged the machine,
    /// so it is opt-in and never a default.
    unsafe_all_live: bool,
    settle_secs: f64,
    /// Override the preflight floor, in bytes.
    ///
    /// Exists so a deliberately small run is possible on a machine that cannot
    /// clear the default 2 GB, without weakening the default for everyone. It
    /// lowers the bar; it does not remove it, and the pressure abort stays
    /// armed either way.
    min_available: Option<u64>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            tabs: 12,
            live: 4,
            url: "https://example.com".into(),
            unsafe_all_live: false,
            settle_secs: 2.5,
            min_available: None,
        }
    }
}

/// Parse `512M`, `2G`, or a bare byte count.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024),
        _ => (s, 1),
    };
    num.parse::<f64>()
        .map(|n| (n * mult as f64) as u64)
        .map_err(|_| format!("bad size `{s}`"))
}

fn parse() -> Result<Args, String> {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--tabs" => a.tabs = it.next().ok_or("--tabs needs a value")?.parse().map_err(|_| "bad --tabs")?,
            "--live" => a.live = it.next().ok_or("--live needs a value")?.parse().map_err(|_| "bad --live")?,
            "--url" => a.url = it.next().ok_or("--url needs a value")?.clone(),
            "--settle" => a.settle_secs = it.next().ok_or("--settle needs a value")?.parse().map_err(|_| "bad --settle")?,
            "--unsafe-all-live" => a.unsafe_all_live = true,
            "--min-available" => {
                let v = it.next().ok_or("--min-available needs a value, e.g. 1G")?;
                a.min_available = Some(parse_size(v)?);
            }
            "-h" | "--help" => {
                println!("memspike [--tabs N] [--live M] [--url URL] [--settle S]");
                println!("         [--min-available SIZE] [--unsafe-all-live]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    if a.live == 0 || a.live > a.tabs {
        return Err("--live must be between 1 and --tabs".into());
    }
    Ok(a)
}

/// Pump the run loop so WebKit can actually make progress.
///
/// Wrapped in an autorelease pool. AppKit's own `run` loop drains one per
/// iteration; a hand-driven `runUntilDate` does not, so without this every
/// autoreleased WebKit object accumulates for the life of the process and
/// nothing is ever actually released.
fn pump(seconds: f64) {
    objc2::rc::autoreleasepool(|_| {
        let until = NSDate::dateWithTimeIntervalSinceNow(seconds);
        NSRunLoop::currentRunLoop().runUntilDate(&until);
    });
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("memspike: {e}");
            std::process::exit(2);
        }
    };

    // ---- preflight -------------------------------------------------------
    let mut guard = Guard::default();
    if let Some(min) = args.min_available {
        println!("\x1b[33mwarning:\x1b[0m preflight floor lowered to {} (default {})",
            human(min), human(guard.required_available));
        guard.required_available = min;
    }
    let mem = match guard.check() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("memspike: refusing to start — {e}");
            eprintln!("  close something, lower --tabs/--live, or pass --min-available to override");
            std::process::exit(1);
        }
    };
    println!("preflight  available {}  swap {} / {}",
        human(mem.available), human(mem.swap_used), human(mem.swap_total));

    if args.unsafe_all_live {
        println!("\x1b[33mwarning:\x1b[0m --unsafe-all-live holds {} web views at once", args.tabs);
    }

    // ---- pressure abort --------------------------------------------------
    let aborted = Arc::new(AtomicBool::new(false));
    let flag = aborted.clone();
    let _monitor = PressureMonitor::start(Some(Box::new(move |p: Pressure| {
        if p != Pressure::Normal {
            flag.store(true, Ordering::SeqCst);
        }
    })));

    let mtm = MainThreadMarker::new().expect("main thread");
    // A bare Mach-O cannot host WKWebView; `cargo xtask spike` bundles this.
    let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);

    // Snapshot WebKit helpers that are not ours before creating any view.
    let census = WebKitCensus::baseline();
    let mut engine = WebKitEngine::headless(mtm);

    let check_abort = |engine: &mut WebKitEngine, ids: &[TabId]| {
        if aborted.load(Ordering::SeqCst) {
            eprintln!("\n\x1b[31mabort:\x1b[0m system memory pressure — tearing down");
            for id in ids {
                engine.destroy(*id);
            }
            std::process::exit(1);
        }
    };

    let ids: Vec<TabId> = (1..=args.tabs as u64).map(TabId).collect();
    let live_cap = if args.unsafe_all_live { args.tabs } else { args.live };

    println!("\nrun        {} tabs, {} live, {}", args.tabs, live_cap, args.url);
    println!("{}", "-".repeat(64));

    // ---- fill --------------------------------------------------------------
    let mut blobs: Vec<(TabId, usize)> = Vec::new();
    let mut live: Vec<TabId> = Vec::new();

    for (i, id) in ids.iter().enumerate() {
        if let Err(e) = engine.load(*id, &args.url) {
            eprintln!("memspike: load {id} failed: {e}");
            std::process::exit(1);
        }
        live.push(*id);
        pump(args.settle_secs);
        check_abort(&mut engine, &ids);

        // Shed the coldest until the live set fits, exactly as the policy does.
        while live.len() > live_cap {
            let victim = live.remove(0);
            match engine.hibernate(victim) {
                Ok(bytes) => blobs.push((victim, bytes)),
                Err(e) => eprintln!("memspike: hibernate {victim} failed: {e}"),
            }
        }
        print!("\r  loaded {}/{}  live {}  hibernated {}   ",
            i + 1, args.tabs, engine.live_count(), engine.session_count());
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    println!();

    let filled = sample(&census, 0);
    println!("\nsteady state");
    println!("  live tabs        {}", engine.live_count());
    println!("  hibernated tabs  {}", engine.session_count());
    println!("  own footprint    {}", human(filled.own_bytes));
    println!("  webkit helpers   {} across {} processes",
        human(filled.helper_bytes), filled.helper_count);
    println!("  total            {}", human(filled.total_bytes()));

    // ---- the gate ----------------------------------------------------------
    println!("\nhibernation cost (gate: {} per tab)", human(HIBERNATION_GATE as u64));
    if blobs.is_empty() {
        println!("  no tabs were hibernated at this --live setting");
    } else {
        let sizes: Vec<usize> = blobs.iter().map(|(_, b)| *b).collect();
        let total: usize = sizes.iter().sum();
        let mut sorted = sizes.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        let max = *sorted.last().unwrap();
        let min = sorted[0];
        println!("  sessions         {}", sizes.len());
        println!("  min / med / max  {} / {} / {}", human(min as u64), human(median as u64), human(max as u64));
        println!("  mean             {}", human((total / sizes.len()) as u64));
        println!("  aggregate        {}", human(total as u64));
        let verdict = if max < HIBERNATION_GATE { "\x1b[32mPASS\x1b[0m" } else { "\x1b[31mFAIL\x1b[0m" };
        println!("  worst vs gate    {}  ({} vs {})", verdict, human(max as u64), human(HIBERNATION_GATE as u64));
        // A number this far under the gate is the interesting result, so state
        // the margin rather than leaving it to be worked out.
        if max > 0 {
            println!("  margin           {:.0}x", HIBERNATION_GATE as f64 / max as f64);
        }
    }

    // ---- restore fidelity --------------------------------------------------
    if let Some((victim, _)) = blobs.first().copied() {
        print!("\nrestore        {victim} ... ");
        match engine.restore(victim) {
            Ok(()) => {
                pump(args.settle_secs);
                println!("live={} ok", engine.is_live(victim));
            }
            Err(e) => println!("\x1b[31mfailed\x1b[0m: {e}"),
        }
    }

    // ---- teardown and reclaim ---------------------------------------------
    println!("\nteardown");
    for id in &ids {
        engine.destroy(*id);
    }
    assert_eq!(engine.live_count(), 0, "every web view must be released");
    assert_eq!(engine.session_count(), 0, "every session must be released");

    // WebKit parks suspended content processes in a reuse cache for ~5 s. A
    // single sample here reports "no reclaim" and is simply wrong.
    println!("\nreclaim (phys_footprint, sampled on a schedule)");
    println!("  {:>5}  {:>12}  {:>12}  {:>8}", "t", "own", "helpers", "procs");
    let mut prev = 0u64;
    for (i, secs) in RECLAIM_SCHEDULE_SECS.iter().enumerate() {
        if i > 0 {
            pump((secs - prev) as f64);
        }
        prev = *secs;
        let s = sample(&census, *secs);
        println!("  {:>4}s  {:>12}  {:>12}  {:>8}",
            secs, human(s.own_bytes), human(s.helper_bytes), s.helper_count);
    }

    // Break the survivors down by helper type. "11 processes remain" is not a
    // finding; "11 WebContent processes remain" names the cause.
    {
        use std::collections::BTreeMap;
        let mut by_kind: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for pid in census.ours() {
            let kind = compact_mem::process_path(pid)
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "unknown".into());
            let e = by_kind.entry(kind).or_default();
            e.0 += 1;
            e.1 += compact_mem::footprint(pid).unwrap_or(0);
        }
        println!("\nsurvivors by kind");
        for (kind, (n, bytes)) in by_kind {
            println!("  {:<34} {:>3}  {:>10}", kind, n, human(bytes));
        }
    }

    // Reuse probe. This is what separates a leak from a cache: if the surviving
    // processes are WebKit's reuse cache, opening fresh tabs should draw from
    // them and the process count should stay flat. If it climbs by one per tab,
    // they were never reusable and the memory is simply lost.
    {
        let before = census.ours().len();
        let probe: Vec<TabId> = (9000..9000 + 3u64).map(TabId).collect();
        for id in &probe {
            let _ = engine.load(*id, &args.url);
            pump(args.settle_secs);
        }
        let peak = census.ours().len();
        for id in &probe {
            engine.destroy(*id);
        }
        pump(2.0);
        println!("\nreuse probe     3 fresh tabs after teardown");
        println!("  processes      {} -> {} ({:+})", before, peak, peak as i64 - before as i64);
        println!("  verdict        {}", if peak <= before {
            "reused — the survivors are a warm cache"
        } else {
            "grew — the survivors were not reusable"
        });
    }

    let end = sample(&census, *RECLAIM_SCHEDULE_SECS.last().unwrap());
    println!("\nidle floor      {} own + {} helpers = {}",
        human(end.own_bytes), human(end.helper_bytes), human(end.total_bytes()));
    println!("reclaimed       {} of {}",
        human(filled.total_bytes().saturating_sub(end.total_bytes())),
        human(filled.total_bytes()));
}
