//! The measured operations. Each `batch(n)` performs `n` operations and checks every result.
//!
//! Setup and teardown happen outside `batch`, so the timed region holds nothing but the
//! operation, its result check, and the loop counter.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, STATUS_ACCESS_VIOLATION, STATUS_SUCCESS, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, VirtualAlloc, VirtualFree,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateThread, GetCurrentProcess, GetCurrentProcessId, PROCESS_BASIC_INFORMATION,
    SetEvent, SignalObjectAndWait, WaitForSingleObject,
};

use super::sys::{Handle, WAIT_MS, last_error, size_u32, wide};

pub(crate) trait Bench {
    /// Performs `n` operations. Any unexpected result returns an error, which aborts the test.
    fn batch(&mut self, n: u64) -> Result<(), String>;
}

/// One test: its name, what a reported unit is, and how to set it up.
pub(crate) struct Spec {
    pub(crate) name: &'static str,
    pub(crate) unit: &'static str,
    /// Reported units per operation: a ping-pong round trip is two context switches.
    pub(crate) units_per_op: u32,
    pub(crate) setup: fn() -> Result<Box<dyn Bench>, String>,
}

pub(crate) const ALL: [Spec; 6] = [
    Spec {
        name: "syscall_shallow",
        unit: "call",
        units_per_op: 1,
        setup: SetSignaledEvent::setup,
    },
    Spec {
        name: "syscall_deep",
        unit: "call",
        units_per_op: 1,
        setup: FileAttributes::setup,
    },
    Spec {
        name: "ctx_switch",
        unit: "switch",
        units_per_op: 2,
        setup: PingPong::setup,
    },
    Spec {
        name: "thread_create",
        unit: "thread",
        units_per_op: 1,
        setup: ThreadCreate::setup,
    },
    Spec {
        name: "kernel_seh_baseline",
        unit: "call",
        units_per_op: 1,
        setup: QueryProcess::setup_valid,
    },
    Spec {
        name: "kernel_seh",
        unit: "call",
        units_per_op: 1,
        setup: QueryProcess::setup_faulting,
    },
];

/// `SetEvent` on a manual-reset event that is already signaled: a short kernel path.
struct SetSignaledEvent {
    event: Handle,
}

impl SetSignaledEvent {
    fn setup() -> Result<Box<dyn Bench>, String> {
        // SAFETY: null attributes and name are allowed; manual reset, initially signaled.
        let raw = unsafe { CreateEventW(ptr::null(), 1, 1, ptr::null()) };
        let event = Handle::new(raw, "CreateEventW")?;
        // SAFETY: `event` is a valid event handle; a zero timeout only polls it.
        let state = unsafe { WaitForSingleObject(event.raw(), 0) };
        if state != WAIT_OBJECT_0 {
            return Err(format!(
                "new event is not signaled (wait returned {state:#x})"
            ));
        }
        Ok(Box::new(Self { event }))
    }
}

impl Bench for SetSignaledEvent {
    fn batch(&mut self, n: u64) -> Result<(), String> {
        let event = self.event.raw();
        for _ in 0..n {
            // SAFETY: `event` stays open for the life of `self`.
            if unsafe { SetEvent(event) } == 0 {
                return Err(last_error("SetEvent"));
            }
        }
        Ok(())
    }
}

/// `GetFileAttributesW` on a small file in %TEMP%: an open through the I/O manager, the
/// filesystem, and every filter driver, so many kernel call/ret pairs per operation.
struct FileAttributes {
    path: PathBuf,
    wide_path: Vec<u16>,
    expected: u32,
}

impl FileAttributes {
    fn setup() -> Result<Box<dyn Bench>, String> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("kcet-bench-{}-{n}.tmp", std::process::id()));
        std::fs::write(&path, [0u8; 64])
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let wide_path = wide(path.as_os_str());
        let mut bench = Self {
            path,
            wide_path,
            expected: INVALID_FILE_ATTRIBUTES,
        };
        // SAFETY: `wide_path` is NUL-terminated UTF-16.
        let expected = unsafe { GetFileAttributesW(bench.wide_path.as_ptr()) };
        if expected == INVALID_FILE_ATTRIBUTES {
            return Err(last_error("GetFileAttributesW"));
        }
        bench.expected = expected;
        Ok(Box::new(bench))
    }
}

impl Bench for FileAttributes {
    fn batch(&mut self, n: u64) -> Result<(), String> {
        let path = self.wide_path.as_ptr();
        for _ in 0..n {
            // SAFETY: `path` points into `self.wide_path`, which is NUL-terminated and outlives the loop.
            let attributes = unsafe { GetFileAttributesW(path) };
            if attributes != self.expected {
                if attributes == INVALID_FILE_ATTRIBUTES {
                    return Err(last_error("GetFileAttributesW"));
                }
                return Err(format!(
                    "attributes changed from {:#x} to {attributes:#x}",
                    self.expected
                ));
            }
        }
        Ok(())
    }
}

impl Drop for FileAttributes {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// State shared with the partner thread. It lives in a leaked box that `PingPong::drop`
/// frees only after the partner has exited.
struct Shared {
    /// Signaled by the partner, waited on by the measuring thread.
    to_main: HANDLE,
    /// Signaled by the measuring thread, waited on by the partner.
    to_partner: HANDLE,
    stop: AtomicBool,
    /// The partner's failing wait result, or 0 while it has not failed.
    partner_failure: AtomicU32,
}

/// Two threads on the same logical CPU hand control back and forth through two
/// auto-reset events with `SignalObjectAndWait`. One round trip is two context switches.
///
/// Both threads inherit the process affinity mask, which `run` sets to one CPU before any
/// test starts, so the switch never crosses CPUs.
struct PingPong {
    shared: *mut Shared,
    thread: Option<Handle>,
}

impl PingPong {
    fn setup() -> Result<Box<dyn Bench>, String> {
        let to_main = new_auto_reset_event()?;
        let to_partner = new_auto_reset_event()?;
        let shared = Box::into_raw(Box::new(Shared {
            to_main: to_main.into_raw(),
            to_partner: to_partner.into_raw(),
            stop: AtomicBool::new(false),
            partner_failure: AtomicU32::new(0),
        }));
        let mut bench = Self {
            shared,
            thread: None,
        };
        // SAFETY: `partner` matches the start routine signature, and `shared` stays valid
        // until `drop` has joined the thread.
        let raw = unsafe {
            CreateThread(
                ptr::null(),
                0,
                Some(partner),
                shared.cast::<c_void>(),
                0,
                ptr::null_mut(),
            )
        };
        bench.thread = Some(Handle::new(raw, "CreateThread")?);
        Ok(Box::new(bench))
    }

    fn shared(&self) -> &Shared {
        // SAFETY: `shared` came from Box::into_raw and is freed only in `drop`.
        unsafe { &*self.shared }
    }
}

fn new_auto_reset_event() -> Result<Handle, String> {
    // SAFETY: null attributes and name are allowed; auto-reset, initially not signaled.
    let raw = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
    Handle::new(raw, "CreateEventW")
}

unsafe extern "system" fn partner(param: *mut c_void) -> u32 {
    // SAFETY: `param` is the `Shared` that `PingPong` keeps alive until this thread exits.
    let shared = unsafe { &*param.cast::<Shared>() };
    // SAFETY: the event handle stays open until this thread has exited.
    let mut result = unsafe { WaitForSingleObject(shared.to_partner, WAIT_MS) };
    while result == WAIT_OBJECT_0 {
        if shared.stop.load(Ordering::Acquire) {
            return 0;
        }
        // SAFETY: both event handles stay open until this thread has exited.
        result = unsafe { SignalObjectAndWait(shared.to_main, shared.to_partner, WAIT_MS, 0) };
    }
    // Keep the value nonzero even if the failure was WAIT_OBJECT_0 (it cannot be here).
    shared
        .partner_failure
        .store(result.max(1), Ordering::Release);
    1
}

impl Bench for PingPong {
    fn batch(&mut self, n: u64) -> Result<(), String> {
        let shared = self.shared();
        let (to_main, to_partner) = (shared.to_main, shared.to_partner);
        for _ in 0..n {
            // SAFETY: both event handles stay open for the life of `self`.
            let result = unsafe { SignalObjectAndWait(to_partner, to_main, WAIT_MS, 0) };
            if result != WAIT_OBJECT_0 {
                let partner = shared.partner_failure.load(Ordering::Acquire);
                let why = if result == u32::MAX {
                    last_error("SignalObjectAndWait")
                } else {
                    format!("SignalObjectAndWait returned {result:#x}")
                };
                return Err(format!("{why}; partner thread failure code {partner:#x}"));
            }
        }
        Ok(())
    }
}

impl Drop for PingPong {
    fn drop(&mut self) {
        let shared = self.shared();
        shared.stop.store(true, Ordering::Release);
        // SAFETY: the event handle is open; waking the partner lets it see `stop` and exit.
        unsafe { SetEvent(shared.to_partner) };
        let joined = self.thread.as_ref().is_none_or(|thread| {
            // SAFETY: the thread handle is open.
            unsafe { WaitForSingleObject(thread.raw(), WAIT_MS) == WAIT_OBJECT_0 }
        });
        if !joined {
            // The partner may still touch `shared`, so leak it rather than free it under the thread.
            return;
        }
        // SAFETY: the partner has exited, so nothing else refers to `shared`.
        let shared = unsafe { Box::from_raw(self.shared) };
        // SAFETY: the events were opened in `setup`, and the partner no longer uses them.
        unsafe { CloseHandle(shared.to_main) };
        // SAFETY: as above.
        unsafe { CloseHandle(shared.to_partner) };
    }
}

/// `CreateThread` with a start routine that returns 0 at once, then wait for it and close it.
struct ThreadCreate;

unsafe extern "system" fn return_zero(_: *mut c_void) -> u32 {
    0
}

impl ThreadCreate {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "every Spec::setup has this signature"
    )]
    fn setup() -> Result<Box<dyn Bench>, String> {
        Ok(Box::new(Self))
    }
}

impl Bench for ThreadCreate {
    fn batch(&mut self, n: u64) -> Result<(), String> {
        for _ in 0..n {
            // SAFETY: `return_zero` matches the start routine signature and ignores its argument.
            let raw = unsafe {
                CreateThread(
                    ptr::null(),
                    0,
                    Some(return_zero),
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                )
            };
            let thread = Handle::new(raw, "CreateThread")?;
            // SAFETY: `thread` is an open thread handle.
            let result = unsafe { WaitForSingleObject(thread.raw(), WAIT_MS) };
            if result != WAIT_OBJECT_0 {
                return Err(format!("waiting for the thread returned {result:#x}"));
            }
            let raw = thread.into_raw();
            // SAFETY: `raw` is open and owned here; closing it is part of the measured operation.
            if unsafe { CloseHandle(raw) } == 0 {
                return Err(last_error("CloseHandle"));
            }
        }
        Ok(())
    }
}

/// `NtQueryInformationProcess(ProcessBasicInformation)` on the current process.
///
/// With a buffer in a committed `PAGE_NOACCESS` page, the kernel's copy-out faults, and the
/// kernel's own try/except unwinds and returns `STATUS_ACCESS_VIOLATION`. With a valid
/// buffer the same call is the baseline.
struct QueryProcess {
    process: HANDLE,
    buffer: *mut c_void,
    /// What `buffer` points to, released on drop.
    memory: Memory,
    expected: i32,
}

enum Memory {
    /// A boxed `PROCESS_BASIC_INFORMATION`, from `Box::into_raw`.
    Valid(*mut PROCESS_BASIC_INFORMATION),
    /// A committed `PAGE_NOACCESS` page from `VirtualAlloc`.
    NoAccess(*mut c_void),
}

const PBI_SIZE: u32 = size_u32::<PROCESS_BASIC_INFORMATION>();
const PAGE: usize = 4096;

impl QueryProcess {
    fn setup_valid() -> Result<Box<dyn Bench>, String> {
        // SAFETY: PROCESS_BASIC_INFORMATION is plain data, valid when zeroed.
        let info: Box<PROCESS_BASIC_INFORMATION> = Box::new(unsafe { std::mem::zeroed() });
        let info = Box::into_raw(info);
        let mut bench = Self {
            // SAFETY: GetCurrentProcess has no preconditions and returns a pseudo-handle.
            process: unsafe { GetCurrentProcess() },
            buffer: info.cast::<c_void>(),
            memory: Memory::Valid(info),
            expected: STATUS_SUCCESS,
        };
        bench.batch(1)?;
        // SAFETY: `info` is live until `bench` drops, and the call above filled it in.
        let reported = unsafe { (*info).UniqueProcessId };
        // SAFETY: GetCurrentProcessId has no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        if reported != pid as usize {
            return Err(format!(
                "query returned process ID {reported}, expected {pid}"
            ));
        }
        Ok(Box::new(bench))
    }

    fn setup_faulting() -> Result<Box<dyn Bench>, String> {
        Ok(Box::new(Self::faulting()?))
    }

    fn faulting() -> Result<Self, String> {
        // SAFETY: a fresh reserve-and-commit of one page; no address is requested.
        let page =
            unsafe { VirtualAlloc(ptr::null(), PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_NOACCESS) };
        if page.is_null() {
            return Err(last_error("VirtualAlloc"));
        }
        Ok(Self {
            // SAFETY: GetCurrentProcess has no preconditions and returns a pseudo-handle.
            process: unsafe { GetCurrentProcess() },
            buffer: page,
            memory: Memory::NoAccess(page),
            expected: STATUS_ACCESS_VIOLATION,
        })
    }
}

impl Bench for QueryProcess {
    fn batch(&mut self, n: u64) -> Result<(), String> {
        let (process, buffer, expected) = (self.process, self.buffer, self.expected);
        for _ in 0..n {
            // SAFETY: `buffer` is either a live PROCESS_BASIC_INFORMATION or a committed
            // no-access page that the kernel probes and faults on; ReturnLength is optional.
            let status = unsafe {
                NtQueryInformationProcess(
                    process,
                    ProcessBasicInformation,
                    buffer,
                    PBI_SIZE,
                    ptr::null_mut(),
                )
            };
            if status != expected {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "NTSTATUS values read best as unsigned hex"
                )]
                let (got, want) = (status as u32, expected as u32);
                return Err(format!(
                    "NtQueryInformationProcess returned {got:#010x}, expected {want:#010x}"
                ));
            }
        }
        Ok(())
    }
}

impl Drop for QueryProcess {
    fn drop(&mut self) {
        match self.memory {
            // SAFETY: `info` came from Box::into_raw and is released only here.
            Memory::Valid(info) => drop(unsafe { Box::from_raw(info) }),
            Memory::NoAccess(page) => {
                // SAFETY: `page` is the base of the allocation made in `faulting`.
                unsafe { VirtualFree(page, 0, MEM_RELEASE) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! These run the real kernel paths on a Windows test machine (CI's Windows job). They
    //! never change process priority or affinity, because tests share the process.

    use super::*;

    fn run(name: &str, n: u64) {
        let spec = ALL.iter().find(|s| s.name == name).unwrap();
        let mut bench = (spec.setup)().unwrap_or_else(|e| panic!("{name} setup: {e}"));
        for _ in 0..3 {
            bench.batch(n).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn syscall_shallow() {
        run("syscall_shallow", 1000);
    }

    #[test]
    fn syscall_deep() {
        run("syscall_deep", 100);
    }

    #[test]
    fn ctx_switch() {
        run("ctx_switch", 1000);
    }

    #[test]
    fn thread_create() {
        run("thread_create", 20);
    }

    #[test]
    fn kernel_seh_baseline_succeeds() {
        run("kernel_seh_baseline", 1000);
    }

    #[test]
    fn kernel_seh_faults_with_access_violation() {
        // batch() fails unless every call returns exactly STATUS_ACCESS_VIOLATION.
        run("kernel_seh", 1000);
    }

    #[test]
    fn unexpected_status_aborts() {
        let mut bench = QueryProcess::faulting().unwrap();
        bench.expected = STATUS_SUCCESS;
        let err = bench.batch(1).unwrap_err();
        assert!(err.contains("0xc0000005"), "{err}");
    }

    #[test]
    fn every_name_is_unique() {
        for (i, a) in ALL.iter().enumerate() {
            assert!(ALL[i + 1..].iter().all(|b| b.name != a.name));
        }
    }
}
