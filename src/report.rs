//! The JSON file one run writes, and the pure helpers that fill it in.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::guard::DeviceGuard;
use crate::stats::Summary;

/// Bumped when a field changes meaning or disappears.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Which kernel stack protection setting a run was taken under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Label {
    Off,
    On,
}

impl Label {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::On => "on",
            Self::Off => "off",
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RunReport {
    pub(crate) schema_version: u32,
    pub(crate) tool_version: String,
    pub(crate) label: Label,
    pub(crate) system: SystemInfo,
    pub(crate) config: RunConfig,
    pub(crate) tests: Vec<TestResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SystemInfo {
    /// ISO 8601, UTC.
    pub(crate) timestamp: String,
    pub(crate) unix_time_ms: u64,
    /// Milliseconds since boot when the run started. `unix_time_ms - uptime_ms` identifies the boot.
    pub(crate) uptime_ms: u64,
    pub(crate) windows: WindowsVersion,
    pub(crate) cpu_brand: String,
    /// `CPUID.(EAX=7,ECX=0):ECX.CET_SS`: the CPU supports shadow stacks.
    pub(crate) cpu_shadow_stack_support: bool,
    pub(crate) logical_cpus: u32,
    pub(crate) power_scheme: PowerScheme,
    pub(crate) device_guard: DeviceGuard,
    /// User-mode shadow stacks for this process. A run refuses to start when this is on.
    pub(crate) user_shadow_stack: bool,
}

impl SystemInfo {
    pub(crate) fn boot_time_ms(&self) -> u64 {
        self.unix_time_ms.saturating_sub(self.uptime_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WindowsVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
    pub(crate) build: u32,
    /// Update build revision from the registry; changes with each cumulative update.
    pub(crate) ubr: Option<u32>,
}

impl fmt::Display for WindowsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)?;
        if let Some(ubr) = self.ubr {
            write!(f, ".{ubr}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PowerScheme {
    pub(crate) guid: String,
    /// Name of a built-in scheme, `None` for a custom one.
    pub(crate) name: Option<String>,
}

impl PowerScheme {
    pub(crate) fn from_guid(guid: String) -> Self {
        let name = match guid.as_str() {
            "381b4222-f694-41f0-9685-ff5bb260df2e" => Some("Balanced"),
            "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c" => Some("High performance"),
            "a1841308-3541-4fab-bc81-f71556f20b4a" => Some("Power saver"),
            "e9a42b02-d5df-448d-aa00-03f14749eb61" => Some("Ultimate Performance"),
            _ => None,
        };
        Self {
            guid,
            name: name.map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunConfig {
    /// The logical CPU every measuring thread was pinned to.
    pub(crate) cpu: u32,
    pub(crate) qpc_frequency: u64,
    pub(crate) warmup_batches: usize,
    pub(crate) batches: usize,
    pub(crate) batch_min_ms: u64,
    pub(crate) batch_target_ms: u64,
    pub(crate) batch_max_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TestResult {
    pub(crate) name: String,
    #[serde(flatten)]
    pub(crate) outcome: Outcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum Outcome {
    Ok {
        /// What one reported unit is: "call", "switch", or "thread".
        unit: String,
        ops_per_batch: u64,
        median_batch_ms: f64,
        /// Measured batches that fell outside the sizing window, from interrupts or frequency changes.
        batches_outside_window: usize,
        ns_per_unit: Summary,
        batch_ns_per_unit: Vec<f64>,
    },
    Aborted {
        reason: String,
    },
}

impl TestResult {
    /// The run median in ns per unit, or `None` for an aborted test.
    pub(crate) fn median_ns(&self) -> Option<f64> {
        match &self.outcome {
            Outcome::Ok { ns_per_unit, .. } => Some(ns_per_unit.median),
            Outcome::Aborted { .. } => None,
        }
    }
}

/// Formats a UTC calendar time as ISO 8601 with milliseconds.
pub(crate) fn iso8601(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
) -> String {
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Converts a Windows `FILETIME` (100 ns ticks since 1601-01-01) to Unix milliseconds.
pub(crate) fn filetime_to_unix_ms(filetime: u64) -> u64 {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    filetime.saturating_sub(UNIX_EPOCH_AS_FILETIME) / 10_000
}

/// Formats GUID fields in the registry's lowercase 8-4-4-4-12 form.
pub(crate) fn format_guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> String {
    let mut out = format!("{data1:08x}-{data2:04x}-{data3:04x}-");
    for (i, b) in data4.iter().enumerate() {
        if i == 2 {
            out.push('-');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamp() {
        assert_eq!(
            iso8601(2026, 11, 3, 9, 5, 7, 42),
            "2026-11-03T09:05:07.042Z"
        );
    }

    #[test]
    fn converts_filetime() {
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000), 0);
        // 2026-01-01T00:00:00Z
        assert_eq!(
            filetime_to_unix_ms(134_116_992_000_000_000),
            1_767_225_600_000
        );
    }

    #[test]
    fn formats_guid_and_names_builtin_schemes() {
        let guid = format_guid(
            0x381b_4222,
            0xf694,
            0x41f0,
            [0x96, 0x85, 0xff, 0x5b, 0xb2, 0x60, 0xdf, 0x2e],
        );
        assert_eq!(guid, "381b4222-f694-41f0-9685-ff5bb260df2e");
        assert_eq!(
            PowerScheme::from_guid(guid).name.as_deref(),
            Some("Balanced")
        );
        assert_eq!(
            PowerScheme::from_guid("00000000-0000-0000-0000-000000000000".into()).name,
            None
        );
    }

    #[test]
    fn outcome_round_trips_with_status_tag() {
        let r = TestResult {
            name: "x".into(),
            outcome: Outcome::Aborted {
                reason: "bad".into(),
            },
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"name":"x","status":"aborted","reason":"bad"}"#);
        let back: TestResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.median_ns(), None);
    }
}
