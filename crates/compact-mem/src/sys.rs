//! Raw bindings to the darwin memory APIs we depend on.
//!
//! Hand-written rather than pulled from a crate: this is four functions and two
//! structs, and the ABI details here (particularly `rusage_info_v0`'s layout)
//! are exactly the thing worth being able to read in-tree when a number looks
//! wrong.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

/// `rusage_info_v0` from `<sys/resource.h>`.
///
/// Only `ri_phys_footprint` is load-bearing, but the struct must be laid out in
/// full because the kernel writes the whole thing.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct RUsageInfoV0 {
    pub ri_uuid: [u8; 16],
    pub ri_user_time: u64,
    pub ri_system_time: u64,
    pub ri_pkg_idle_wkups: u64,
    pub ri_interrupt_wkups: u64,
    pub ri_pageins: u64,
    pub ri_wired_size: u64,
    pub ri_resident_size: u64,
    /// The number that matters. See the module docs on `footprint`.
    pub ri_phys_footprint: u64,
    pub ri_proc_start_abstime: u64,
    pub ri_proc_exit_abstime: u64,
}

pub const RUSAGE_INFO_V0: c_int = 0;
pub const PROC_ALL_PIDS: u32 = 1;
pub const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

/// `vm_statistics64` from `<mach/vm_statistics.h>`, truncated after the fields
/// we read. `host_statistics64` is told the exact count, so the tail is never
/// written.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct VMStatistics64 {
    pub free_count: c_uint,
    pub active_count: c_uint,
    pub inactive_count: c_uint,
    pub wire_count: c_uint,
    pub zero_fill_count: u64,
    pub reactivations: u64,
    pub pageins: u64,
    pub pageouts: u64,
    pub faults: u64,
    pub cow_faults: u64,
    pub lookups: u64,
    pub hits: u64,
    pub purges: u64,
    pub purgeable_count: c_uint,
    pub speculative_count: c_uint,
    pub decompressions: u64,
    pub compressions: u64,
    pub swapins: u64,
    pub swapouts: u64,
    pub compressor_page_count: c_uint,
    pub throttled_count: c_uint,
    pub external_page_count: c_uint,
    pub internal_page_count: c_uint,
    pub total_uncompressed_pages_in_compressor: u64,
}

/// `struct xsw_usage` from `<sys/sysctl.h>`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct XswUsage {
    pub xsu_total: u64,
    pub xsu_avail: u64,
    pub xsu_used: u64,
    pub xsu_pagesize: u32,
    pub xsu_encrypted: u8,
}

pub const HOST_VM_INFO64: c_int = 4;
/// `HOST_VM_INFO64_COUNT` — size of [`VMStatistics64`] in 32-bit words.
pub const HOST_VM_INFO64_COUNT: c_uint =
    (std::mem::size_of::<VMStatistics64>() / std::mem::size_of::<c_uint>()) as c_uint;

extern "C" {
    pub fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    pub fn proc_listpids(ty: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int) -> c_int;
    pub fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;

    pub fn mach_host_self() -> c_uint;
    pub fn host_statistics64(
        host: c_uint,
        flavor: c_int,
        host_info: *mut c_void,
        host_info_count: *mut c_uint,
    ) -> c_int;
    pub fn host_page_size(host: c_uint, out_page_size: *mut usize) -> c_int;

    pub fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
}
