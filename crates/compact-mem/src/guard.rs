//! Preflight and runtime guards.
//!
//! This exists because a memory spike wedged the development machine on
//! 2026-09-03 — not "failed", wedged: an 8 GB M1 already holding ~5 GB of
//! Chrome and 3.8 GB of swap does not degrade gracefully when something asks
//! for thirty web content processes. Headroom is never assumed.

use crate::sys;
use std::ffi::{c_uint, c_void, CString};

/// A reading of system-wide memory conditions.
#[derive(Debug, Clone, Copy)]
pub struct SystemMemory {
    /// Bytes plausibly available without swapping.
    pub available: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub page_size: usize,
}

impl SystemMemory {
    pub fn read() -> Option<Self> {
        let host = unsafe { sys::mach_host_self() };

        let mut page_size: usize = 0;
        if unsafe { sys::host_page_size(host, &mut page_size) } != 0 || page_size == 0 {
            return None;
        }

        let mut vm = sys::VMStatistics64::default();
        let mut count: c_uint = sys::HOST_VM_INFO64_COUNT;
        let rc = unsafe {
            sys::host_statistics64(
                host,
                sys::HOST_VM_INFO64,
                &mut vm as *mut _ as *mut c_void,
                &mut count,
            )
        };
        if rc != 0 {
            return None;
        }

        // Free plus reclaimable. Inactive and purgeable pages can be handed
        // over without paging out, so excluding them would understate headroom
        // badly on a machine that has been running a while. Speculative pages
        // are deliberately excluded — they are cheap to drop but counting them
        // makes the guard optimistic, and this guard's whole job is pessimism.
        let reclaimable =
            vm.free_count as u64 + vm.inactive_count as u64 + vm.purgeable_count as u64;

        let mut swap = sys::XswUsage::default();
        let mut len = std::mem::size_of::<sys::XswUsage>();
        let name = CString::new("vm.swapusage").ok()?;
        let swap_ok = unsafe {
            sys::sysctlbyname(
                name.as_ptr(),
                &mut swap as *mut _ as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        } == 0;

        Some(Self {
            available: reclaimable * page_size as u64,
            swap_used: if swap_ok { swap.xsu_used } else { 0 },
            swap_total: if swap_ok { swap.xsu_total } else { 0 },
            page_size,
        })
    }
}

#[derive(Debug)]
pub enum GuardError {
    /// System memory could not be read; refusing to guess.
    Unreadable,
    NotEnoughMemory { available: u64, required: u64 },
    DeepInSwap { swap_used: u64, limit: u64 },
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::Unreadable => {
                write!(f, "could not read system memory state; refusing to run blind")
            }
            GuardError::NotEnoughMemory { available, required } => write!(
                f,
                "only {} available, need {}",
                crate::human(*available),
                crate::human(*required)
            ),
            GuardError::DeepInSwap { swap_used, limit } => write!(
                f,
                "{} of swap already in use (limit {}) — the machine is already thrashing",
                crate::human(*swap_used),
                crate::human(*limit)
            ),
        }
    }
}

impl std::error::Error for GuardError {}

/// Preflight check run before anything allocates.
#[derive(Debug, Clone, Copy)]
pub struct Guard {
    pub required_available: u64,
    pub max_swap_used: u64,
}

impl Default for Guard {
    fn default() -> Self {
        Self {
            // 2 GB free before we start. Below this the M0 harness is not
            // measuring hibernation, it is measuring swap.
            required_available: 2 * 1024 * 1024 * 1024,
            // Some swap in use is normal on macOS; 6 GB means the machine is
            // already in trouble and our run will make it worse.
            max_swap_used: 6 * 1024 * 1024 * 1024,
        }
    }
}

impl Guard {
    pub fn check(&self) -> Result<SystemMemory, GuardError> {
        let mem = SystemMemory::read().ok_or(GuardError::Unreadable)?;
        if mem.available < self.required_available {
            return Err(GuardError::NotEnoughMemory {
                available: mem.available,
                required: self.required_available,
            });
        }
        if mem.swap_used > self.max_swap_used {
            return Err(GuardError::DeepInSwap {
                swap_used: mem.swap_used,
                limit: self.max_swap_used,
            });
        }
        Ok(mem)
    }
}
