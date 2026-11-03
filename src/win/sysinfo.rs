//! The system description stored with every run.

use std::ffi::c_void;
use std::process::Command;
use std::ptr;

use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, FILETIME, LocalFree, STATUS_SUCCESS, SYSTEMTIME,
};
use windows_sys::Win32::System::Power::PowerGetActiveScheme;
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemTime, GetSystemTimeAsFileTime, GetTickCount64, OSVERSIONINFOW,
};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, GetProcessMitigationPolicy,
    ProcessUserShadowStackPolicy,
};
use windows_sys::core::GUID;

use super::sys::{last_error, size_u32, wide};
use crate::guard::{self, DeviceGuard};
use crate::report::{self, PowerScheme, WindowsVersion};

/// Wall-clock time and uptime, read together.
pub(crate) struct Now {
    pub(crate) timestamp: String,
    pub(crate) unix_time_ms: u64,
    pub(crate) uptime_ms: u64,
}

pub(crate) fn now() -> Now {
    let mut st = SYSTEMTIME::default();
    // SAFETY: `st` is a valid, writable SYSTEMTIME.
    unsafe { GetSystemTime(&raw mut st) };
    let mut ft = FILETIME::default();
    // SAFETY: `ft` is a valid, writable FILETIME.
    unsafe { GetSystemTimeAsFileTime(&raw mut ft) };
    // SAFETY: GetTickCount64 has no preconditions.
    let uptime_ms = unsafe { GetTickCount64() };
    Now {
        timestamp: report::iso8601(
            st.wYear,
            st.wMonth,
            st.wDay,
            st.wHour,
            st.wMinute,
            st.wSecond,
            st.wMilliseconds,
        ),
        unix_time_ms: report::filetime_to_unix_ms(
            u64::from(ft.dwHighDateTime) << 32 | u64::from(ft.dwLowDateTime),
        ),
        uptime_ms,
    }
}

pub(crate) fn windows_version() -> Result<WindowsVersion, String> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_u32::<OSVERSIONINFOW>(),
        ..Default::default()
    };
    // SAFETY: `info` is writable and its size field is set, as RtlGetVersion requires.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    if status != STATUS_SUCCESS {
        return Err(format!("RtlGetVersion returned {status:#x}"));
    }
    Ok(WindowsVersion {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
        ubr: update_build_revision(),
    })
}

/// The `UBR` value under `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`, if readable.
fn update_build_revision() -> Option<u32> {
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion".as_ref());
    let name = wide("UBR".as_ref());
    let mut value = 0u32;
    let mut size = size_u32::<u32>();
    // SAFETY: the strings are NUL-terminated, and `value`/`size` describe a writable u32.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut value).cast::<c_void>(),
            &raw mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}

/// The CPUID brand string, such as "13th Gen Intel(R) Core(TM) i7-13700K".
pub(crate) fn cpu_brand() -> String {
    use std::arch::x86_64::__cpuid;
    if __cpuid(0x8000_0000).eax < 0x8000_0004 {
        return String::from("unknown");
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004 {
        let r = __cpuid(leaf);
        for reg in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&reg.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&bytes)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_owned()
}

/// `CPUID.(EAX=7,ECX=0):ECX` bit 7 (`CET_SS`): the CPU supports shadow stacks.
pub(crate) fn cpu_shadow_stack_support() -> bool {
    use std::arch::x86_64::{__cpuid, __cpuid_count};
    __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ecx & (1 << 7) != 0
}

pub(crate) fn logical_cpus() -> u32 {
    // SAFETY: GetActiveProcessorCount has no preconditions.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

pub(crate) fn power_scheme() -> Result<PowerScheme, String> {
    let mut guid: *mut GUID = ptr::null_mut();
    // SAFETY: a null root key selects the system default; `guid` receives an allocation.
    let status = unsafe { PowerGetActiveScheme(ptr::null_mut(), &raw mut guid) };
    if status != ERROR_SUCCESS || guid.is_null() {
        return Err(format!("PowerGetActiveScheme failed (error {status})"));
    }
    // SAFETY: on success `guid` points to a GUID that we own.
    let g = unsafe { *guid };
    // SAFETY: PowerGetActiveScheme documents LocalFree for the returned GUID.
    unsafe { LocalFree(guid.cast::<c_void>()) };
    Ok(PowerScheme::from_guid(report::format_guid(
        g.data1, g.data2, g.data3, g.data4,
    )))
}

/// Whether user-mode shadow stacks are on for this process. The binary is not built with
/// /CETCOMPAT, so they should be off unless a system-wide policy forces them on.
pub(crate) fn user_shadow_stack() -> Result<bool, String> {
    // PROCESS_MITIGATION_USER_SHADOW_STACK_POLICY is one DWORD of flags;
    // EnableUserShadowStack is bit 0.
    let mut flags = 0u32;
    // SAFETY: GetCurrentProcess has no preconditions and returns a pseudo-handle.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: the buffer is a writable DWORD, the documented size of this policy.
    let ok = unsafe {
        GetProcessMitigationPolicy(
            process,
            ProcessUserShadowStackPolicy,
            (&raw mut flags).cast::<c_void>(),
            size_of::<u32>(),
        )
    };
    if ok == 0 {
        return Err(last_error(
            "GetProcessMitigationPolicy(ProcessUserShadowStackPolicy)",
        ));
    }
    Ok(flags & 1 != 0)
}

/// Queries `Win32_DeviceGuard` through PowerShell, printing plain lines rather than JSON
/// because Windows PowerShell 5.1 serializes one-element and empty arrays inconsistently.
pub(crate) fn device_guard() -> Result<DeviceGuard, String> {
    const SCRIPT: &str = "$ErrorActionPreference = 'Stop'; \
        $d = Get-CimInstance -Namespace root\\Microsoft\\Windows\\DeviceGuard -ClassName Win32_DeviceGuard; \
        ($d.SecurityServicesRunning -join ','); \
        [string]$d.VirtualizationBasedSecurityStatus";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .output()
        .map_err(|e| format!("cannot start powershell: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "querying Win32_DeviceGuard failed ({}). Try an elevated PowerShell. {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    guard::parse(&String::from_utf8_lossy(&output.stdout))
}
