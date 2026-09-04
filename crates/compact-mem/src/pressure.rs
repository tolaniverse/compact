//! Memory pressure monitoring via `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`.
//!
//! Polling would be both late and wasteful: the kernel already knows when
//! things are tight and will tell us. A pressure event is the signal that turns
//! Compact's live-set budget from a static number into something that yields
//! when the rest of the machine needs the memory more.
//!
//! The handler is registered as a bare `extern "C"` function with a context
//! pointer rather than a block, which keeps this free of any block runtime.

use compact_core::policy::Pressure;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const DISPATCH_MEMORYPRESSURE_NORMAL: usize = 0x01;
const DISPATCH_MEMORYPRESSURE_WARN: usize = 0x02;
const DISPATCH_MEMORYPRESSURE_CRITICAL: usize = 0x04;

#[allow(non_camel_case_types)]
type dispatch_object_t = *mut c_void;

extern "C" {
    static _dispatch_source_type_memorypressure: [u8; 0];

    fn dispatch_source_create(
        ty: *const c_void,
        handle: usize,
        mask: usize,
        queue: dispatch_object_t,
    ) -> dispatch_object_t;
    fn dispatch_source_set_event_handler_f(
        source: dispatch_object_t,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_source_get_data(source: dispatch_object_t) -> usize;
    fn dispatch_set_context(object: dispatch_object_t, context: *mut c_void);
    fn dispatch_resume(object: dispatch_object_t);
    fn dispatch_source_cancel(object: dispatch_object_t);
    fn dispatch_release(object: dispatch_object_t);
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> dispatch_object_t;
}

/// Shared, lock-free pressure level. Read from the UI thread on every
/// reconcile; written from the dispatch queue.
#[derive(Debug, Default)]
pub struct PressureCell(AtomicU8);

impl PressureCell {
    pub fn get(&self) -> Pressure {
        match self.0.load(Ordering::Relaxed) {
            2 => Pressure::Critical,
            1 => Pressure::Warn,
            _ => Pressure::Normal,
        }
    }

    fn set(&self, p: Pressure) {
        let v = match p {
            Pressure::Normal => 0,
            Pressure::Warn => 1,
            Pressure::Critical => 2,
        };
        self.0.store(v, Ordering::Relaxed);
    }
}

extern "C" fn on_pressure(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the raw pointer produced by `Arc::into_raw` in `start`,
    // which keeps the allocation alive for as long as the source exists. We
    // borrow it without taking ownership, so the refcount is untouched.
    let state = unsafe { &*(ctx as *const State) };
    let data = unsafe { dispatch_source_get_data(state.source) };
    let level = if data & DISPATCH_MEMORYPRESSURE_CRITICAL != 0 {
        Pressure::Critical
    } else if data & DISPATCH_MEMORYPRESSURE_WARN != 0 {
        Pressure::Warn
    } else if data & DISPATCH_MEMORYPRESSURE_NORMAL != 0 {
        Pressure::Normal
    } else {
        return;
    };
    state.cell.set(level);
    if let Some(cb) = &state.callback {
        cb(level);
    }
}

struct State {
    cell: Arc<PressureCell>,
    source: dispatch_object_t,
    callback: Option<Box<dyn Fn(Pressure) + Send + Sync>>,
}

// SAFETY: `source` is a libdispatch object, which is thread-safe by contract;
// the other fields are already Send + Sync.
unsafe impl Send for State {}
unsafe impl Sync for State {}

/// An active memory-pressure subscription. Dropping it cancels the source.
pub struct PressureMonitor {
    state: *const State,
}

// SAFETY: as for `State` — the pointee is Send + Sync and libdispatch objects
// may be cancelled from any thread.
unsafe impl Send for PressureMonitor {}
unsafe impl Sync for PressureMonitor {}

impl PressureMonitor {
    /// Begin monitoring. `callback` runs on a background dispatch queue, so it
    /// must not touch AppKit directly — hop to the main thread first.
    pub fn start(callback: Option<Box<dyn Fn(Pressure) + Send + Sync>>) -> Option<(Self, Arc<PressureCell>)> {
        let cell = Arc::new(PressureCell::default());
        unsafe {
            let queue = dispatch_get_global_queue(0, 0);
            let source = dispatch_source_create(
                _dispatch_source_type_memorypressure.as_ptr() as *const c_void,
                0,
                DISPATCH_MEMORYPRESSURE_NORMAL
                    | DISPATCH_MEMORYPRESSURE_WARN
                    | DISPATCH_MEMORYPRESSURE_CRITICAL,
                queue,
            );
            if source.is_null() {
                return None;
            }
            let state = Arc::new(State { cell: Arc::clone(&cell), source, callback });
            let raw = Arc::into_raw(state);
            dispatch_set_context(source, raw as *mut c_void);
            dispatch_source_set_event_handler_f(source, on_pressure);
            // Dispatch sources are created suspended.
            dispatch_resume(source);
            Some((Self { state: raw }, cell))
        }
    }
}

impl Drop for PressureMonitor {
    fn drop(&mut self) {
        unsafe {
            let state = &*self.state;
            dispatch_source_cancel(state.source);
            dispatch_release(state.source);
            // Reclaim the refcount handed to libdispatch in `start`.
            drop(Arc::from_raw(self.state));
        }
    }
}
