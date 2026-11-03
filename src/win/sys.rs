//! Small wrappers over the Win32 calls every module needs.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt as _;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Timeout for every wait. Nothing measured comes close; hitting it means a test is stuck.
pub(crate) const WAIT_MS: u32 = 10_000;

/// Formats the calling thread's last Win32 error for a failed `what`.
pub(crate) fn last_error(what: &str) -> String {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    format!("{what} failed (error {code})")
}

/// An owned kernel handle, closed on drop.
pub(crate) struct Handle(HANDLE);

impl Handle {
    /// Takes ownership of `raw`, or reports `what` as failed when it is null.
    pub(crate) fn new(raw: HANDLE, what: &str) -> Result<Self, String> {
        if raw.is_null() {
            Err(last_error(what))
        } else {
            Ok(Self(raw))
        }
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }

    /// Gives up ownership without closing the handle.
    pub(crate) fn into_raw(self) -> HANDLE {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value, which closes it only once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Reads the performance counter.
pub(crate) fn qpc() -> Result<i64, String> {
    let mut now = 0i64;
    // SAFETY: `now` is a valid, writable i64.
    if unsafe { QueryPerformanceCounter(&raw mut now) } == 0 {
        return Err(last_error("QueryPerformanceCounter"));
    }
    Ok(now)
}

/// Performance counter ticks per second.
pub(crate) fn qpc_frequency() -> Result<u64, String> {
    let mut freq = 0i64;
    // SAFETY: `freq` is a valid, writable i64.
    if unsafe { QueryPerformanceFrequency(&raw mut freq) } == 0 {
        return Err(last_error("QueryPerformanceFrequency"));
    }
    u64::try_from(freq)
        .ok()
        .filter(|&f| f > 0)
        .ok_or_else(|| format!("bad QPC frequency {freq}"))
}

/// `size_of::<T>()` as the `u32` that Win32 size parameters take.
pub(crate) const fn size_u32<T>() -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the structs passed to Win32 are far below 4 GiB"
    )]
    let size = size_of::<T>() as u32;
    size
}

/// NUL-terminated UTF-16 for a `PCWSTR` argument.
pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
