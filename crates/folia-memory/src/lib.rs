// Allocator tuning for a long desktop session: a smaller set of heaps, and freed pages returned to the system.

/// Applies the allocator settings Folia wants and keeps freed heap pages from staying resident.
///
/// Call this first in `main`, before the interface starts any threads.
pub fn tune() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::tune();
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    use std::time::Duration;

    /// glibc gives every thread that allocates its own heap, each one reserving 64 MiB and keeping freed pages
    /// resident. Decoding, image loading and text shaping here run on a few dozen short-lived threads, which
    /// uncapped adds up to a gigabyte of mostly empty heap; two heaps are still enough to keep allocation from
    /// serialising.
    const ARENA_MAX: i32 = 2;
    /// Freed blocks at least this large are handed straight back to the system instead of staying in a heap.
    /// 256 KiB is above the cover and lyric buffers that come and go all the time, and below the one-off images
    /// that would otherwise bloat a heap.
    const MMAP_THRESHOLD: i32 = 256 * 1024;
    /// A heap whose top has at least this much free space releases it as soon as the last block there is freed.
    const TRIM_THRESHOLD: i32 = 256 * 1024;
    /// Free pages under live ones cannot be released by the two thresholds; walk the heaps this often.
    const TRIM_EVERY: Duration = Duration::from_secs(30);

    pub(super) fn tune() {
        // Environment variables are the documented way to override these, but a desktop entry does not pass them
        // on. Set the same knobs from inside the process instead, and leave alone any the user did set.
        if unset("MALLOC_ARENA_MAX") {
            mallopt(libc::M_ARENA_MAX, ARENA_MAX);
        }
        if unset("MALLOC_MMAP_THRESHOLD_") {
            mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
        }
        if unset("MALLOC_TRIM_THRESHOLD_") {
            mallopt(libc::M_TRIM_THRESHOLD, TRIM_THRESHOLD);
        }
        // Trimming on `free` only reaches the top of a heap, and a player grows and shrinks several heaps as pages
        // open and close, which strands free pages under live ones. A periodic full trim releases those.
        let _ = std::thread::Builder::new().name("folia-trim".to_owned()).spawn(trim);
    }

    fn unset(name: &str) -> bool {
        std::env::var_os(name).is_none()
    }

    fn mallopt(parameter: i32, value: i32) {
        // SAFETY: `mallopt` only rewrites glibc's own tunables; it takes no pointers, touches no caller memory, and
        // glibc allows it at any time. This runs before the interface creates its threads.
        unsafe {
            libc::mallopt(parameter, value);
        }
    }

    fn trim() {
        loop {
            std::thread::sleep(TRIM_EVERY);
            // SAFETY: `malloc_trim` walks glibc's heap metadata under its own lock and unmaps free pages; it takes
            // no pointers and is safe to call while other threads allocate.
            unsafe {
                libc::malloc_trim(0);
            }
        }
    }
}
