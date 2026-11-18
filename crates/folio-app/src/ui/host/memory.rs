//! 只读取物理内存容量；预算与缓存所有权仍在阅读会话。

pub fn physical_memory_bytes() -> Option<u64> {
    static BYTES: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *BYTES.get_or_init(physical_memory)
}

#[cfg(target_os = "windows")]
fn physical_memory() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    }
    let mut status = MemoryStatus {
        length: std::mem::size_of::<MemoryStatus>() as u32,
        load: 0,
        total_physical: 0,
        available_physical: 0,
        total_page_file: 0,
        available_page_file: 0,
        total_virtual: 0,
        available_virtual: 0,
        available_extended_virtual: 0,
    };
    // SAFETY: repr(C) matches MEMORYSTATUSEX, length is initialized and the
    // writable structure lives until the synchronous call returns.
    let succeeded = unsafe { GlobalMemoryStatusEx(&mut status) } != 0;
    (succeeded && status.total_physical > 0).then_some(status.total_physical)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn physical_memory() -> Option<u64> {
    // SAFETY: sysconf takes constant selectors and writes no caller memory.
    let (pages, page_bytes) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let (pages, page_bytes) = (u64::try_from(pages).ok()?, u64::try_from(page_bytes).ok()?);
    pages.checked_mul(page_bytes).filter(|bytes| *bytes > 0)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn physical_memory() -> Option<u64> {
    let mut bytes = 0u64;
    let mut len = std::mem::size_of_val(&bytes);
    // SAFETY: hw.memsize returns a uint64_t; both output pointers refer to live
    // writable values, and null newp requests a read without changing the OS.
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && len == std::mem::size_of_val(&bytes) && bytes > 0).then_some(bytes)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn physical_memory() -> Option<u64> {
    None
}
