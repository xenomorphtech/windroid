//! Host services: randomness and clocks.

use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub fn random_bytes(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        unsafe {
            BCryptGenRandom(std::ptr::null_mut(), chunk.as_mut_ptr(), chunk.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
        }
    }
}

/// System-wide monotonic clock in nanoseconds (QPC based).
pub fn monotonic_ns() -> u64 {
    let mut c = 0i64;
    let mut f = 0i64;
    unsafe {
        QueryPerformanceCounter(&mut c);
        QueryPerformanceFrequency(&mut f);
    }
    ((c as u128 * 1_000_000_000u128) / f as u128) as u64
}

/// Wall clock: (seconds, nanoseconds) since the Unix epoch.
pub fn realtime() -> (i64, i64) {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos() as i64)
}
