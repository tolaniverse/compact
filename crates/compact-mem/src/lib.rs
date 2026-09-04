//! Memory measurement and safety rails for Compact.
//!
//! Three hard-won rules from the M0 spike are encoded here rather than left to
//! the caller to remember:
//!
//! 1. **Measure `phys_footprint`, never RSS.** RSS counts shared WebKit
//!    framework pages against every process that maps them, which would credit
//!    Compact with memory it never allocated and make the tab budget fiction.
//! 2. **Attribute WebKit helpers by pid-set difference, not parentage.** They
//!    are launchd XPC services with ppid 1, so there is no process tree to walk.
//!    Snapshot the pre-existing `com.apple.WebKit*` pids first and count only
//!    what appears afterwards.
//! 3. **Sample on a schedule, never once.** WebKit parks suspended WebContent
//!    processes in a reuse cache for roughly five seconds after the last web
//!    view goes away, so a reading taken immediately after teardown reports "no
//!    reclaim" and is simply wrong.
//!
//! [`Guard`] exists for a fourth reason, learned the expensive way: an
//! unguarded 30-webview run wedged the development machine on 2026-09-03.

pub mod guard;
pub mod pressure;
pub mod sys;

use std::ffi::c_void;
use std::path::PathBuf;

pub use guard::{Guard, GuardError};
pub use pressure::PressureMonitor;

/// Process identifier.
pub type Pid = i32;

/// Physical footprint of a process, in bytes.
///
/// This is the same accounting the memory-pressure system and jetsam use, and
/// the only per-process number that means "memory this process is responsible
/// for". Returns `None` if the process is gone or not ours to inspect.
pub fn footprint(pid: Pid) -> Option<u64> {
    let mut info = sys::RUsageInfoV0::default();
    let rc = unsafe {
        sys::proc_pid_rusage(
            pid,
            sys::RUSAGE_INFO_V0,
            &mut info as *mut _ as *mut c_void,
        )
    };
    (rc == 0).then_some(info.ri_phys_footprint)
}

/// Executable path for a pid, if readable.
pub fn process_path(pid: Pid) -> Option<PathBuf> {
    let mut buf = vec![0u8; sys::PROC_PIDPATHINFO_MAXSIZE];
    let n = unsafe {
        sys::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32)
    };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok().map(PathBuf::from)
}

/// Every pid on the system we are allowed to see.
pub fn all_pids() -> Vec<Pid> {
    // Ask for the required size, then over-allocate: processes can appear
    // between the two calls, and a short buffer silently truncates.
    let needed = unsafe { sys::proc_listpids(sys::PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }
    let cap = (needed as usize / std::mem::size_of::<Pid>()) + 64;
    let mut buf = vec![0 as Pid; cap];
    let written = unsafe {
        sys::proc_listpids(
            sys::PROC_ALL_PIDS,
            0,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * std::mem::size_of::<Pid>()) as i32,
        )
    };
    if written <= 0 {
        return Vec::new();
    }
    buf.truncate(written as usize / std::mem::size_of::<Pid>());
    buf.retain(|p| *p > 0);
    buf
}

/// Whether a process is one of WebKit's helper services.
///
/// Matches WebContent, Networking and GPU processes alike; all three are real
/// costs of having a live tab.
pub fn is_webkit_helper(pid: Pid) -> bool {
    process_path(pid)
        .and_then(|p| p.to_str().map(|s| s.contains("com.apple.WebKit")))
        .unwrap_or(false)
}

/// A baseline of WebKit helpers that existed *before* we started.
///
/// Any other browser on the machine — and this development machine runs Chrome
/// heavily and Safari occasionally — has its own helpers. Counting them would
/// attribute several gigabytes to Compact.
#[derive(Debug, Clone)]
pub struct WebKitCensus {
    baseline: Vec<Pid>,
}

impl WebKitCensus {
    /// Snapshot the helpers already running. Call this before creating any web
    /// view.
    pub fn baseline() -> Self {
        let mut baseline: Vec<Pid> = all_pids().into_iter().filter(|p| is_webkit_helper(*p)).collect();
        baseline.sort_unstable();
        Self { baseline }
    }

    /// Helpers that appeared since the baseline — ours.
    pub fn ours(&self) -> Vec<Pid> {
        all_pids()
            .into_iter()
            .filter(|p| self.baseline.binary_search(p).is_err() && is_webkit_helper(*p))
            .collect()
    }

    /// Summed footprint of our helpers, plus the count that produced it.
    pub fn total(&self) -> (u64, usize) {
        let pids = self.ours();
        let sum = pids.iter().filter_map(|p| footprint(*p)).sum();
        (sum, pids.len())
    }
}

/// A footprint reading, tagged with when it was taken relative to the event of
/// interest.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Seconds since the triggering event (teardown, load, etc).
    pub at_secs: u64,
    /// Our own process footprint.
    pub own_bytes: u64,
    /// Summed footprint of WebKit helpers attributable to us.
    pub helper_bytes: u64,
    pub helper_count: usize,
}

impl Sample {
    pub fn total_bytes(&self) -> u64 {
        self.own_bytes + self.helper_bytes
    }
}

/// The sampling schedule that survives WebKit's process reuse cache.
///
/// The 5 s entry is the important one: it is just past the cache's hold time,
/// so it is the first reading that can legitimately show reclaim. Sampling only
/// at 0 s produces a confident, repeatable, wrong answer.
pub const RECLAIM_SCHEDULE_SECS: [u64; 4] = [0, 5, 15, 30];

/// Take a reading now, labelled `at_secs` since the triggering event.
pub fn sample(census: &WebKitCensus, at_secs: u64) -> Sample {
    let (helper_bytes, helper_count) = census.total();
    Sample {
        at_secs,
        own_bytes: footprint(std::process::id() as Pid).unwrap_or(0),
        helper_bytes,
        helper_count,
    }
}

/// Format bytes the way the settings footer and the spike report do.
pub fn human(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_our_own_footprint() {
        let f = footprint(std::process::id() as Pid).expect("own footprint must be readable");
        // A running test binary is at least a megabyte and nowhere near 100 GB;
        // this catches a struct-layout error reading the wrong field.
        assert!(f > 1024 * 1024, "implausibly small: {f}");
        assert!(f < 100 * 1024 * 1024 * 1024, "implausibly large: {f}");
    }

    #[test]
    fn footprint_of_a_dead_pid_is_none() {
        assert!(footprint(-1).is_none());
    }

    #[test]
    fn enumerates_processes_and_resolves_our_path() {
        let pids = all_pids();
        assert!(pids.len() > 10, "a live macOS system has many processes");
        let me = std::process::id() as Pid;
        assert!(pids.contains(&me));
        assert!(process_path(me).is_some());
    }

    #[test]
    fn reads_system_memory() {
        let m = guard::SystemMemory::read().expect("host_statistics64 must work");
        assert!(m.page_size >= 4096);
        assert!(m.available > 0);
        assert!(m.swap_used <= m.swap_total || m.swap_total == 0);
    }

    #[test]
    fn census_excludes_pre_existing_helpers() {
        // Whatever WebKit helpers exist right now belong to other apps, so a
        // fresh census must attribute nothing to us.
        let census = WebKitCensus::baseline();
        let (bytes, count) = census.total();
        assert_eq!(count, 0, "baseline must not claim other apps' helpers");
        assert_eq!(bytes, 0);
    }

    #[test]
    fn human_formats_at_each_scale() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2_355), "2.3 KB"); // the M0 hibernated-tab figure
        assert_eq!(human(58 * 1024 * 1024), "58.0 MB");
        assert_eq!(human(2 * 1024 * 1024 * 1024), "2.00 GB");
    }

    #[test]
    fn the_schedule_clears_webkits_reuse_cache() {
        assert_eq!(RECLAIM_SCHEDULE_SECS[0], 0);
        assert!(
            RECLAIM_SCHEDULE_SECS[1] >= 5,
            "the second sample must land past the ~5s process reuse cache"
        );
    }
}
