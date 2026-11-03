//! Device Guard state and the rule for which label a run may carry.

use serde::{Deserialize, Serialize};

use crate::report::Label;

/// `SecurityServicesRunning` value for memory integrity (HVCI).
pub(crate) const HVCI: u32 = 2;
/// `SecurityServicesRunning` value for kernel-mode hardware-enforced stack protection.
pub(crate) const KERNEL_STACK_PROTECTION: u32 = 5;
/// `SecurityServicesRunning` value for kernel-mode hardware-enforced stack protection in
/// audit mode. Shadow stacks are active but violations are only logged.
pub(crate) const KERNEL_STACK_PROTECTION_AUDIT: u32 = 6;
/// `VirtualizationBasedSecurityStatus` value for "VBS is running".
pub(crate) const VBS_RUNNING: u32 = 2;

/// The `Win32_DeviceGuard` fields a run depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeviceGuard {
    pub(crate) security_services_running: Vec<u32>,
    pub(crate) virtualization_based_security_status: u32,
}

/// Parses the PowerShell output: a comma-separated `SecurityServicesRunning` line (empty when
/// no service runs, which PowerShell may print as a blank line or not at all) followed by
/// the `VirtualizationBasedSecurityStatus` line.
pub(crate) fn parse(output: &str) -> Result<DeviceGuard, String> {
    let mut lines: Vec<&str> = output.lines().map(str::trim).collect();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    let (services, vbs) = match lines.as_slice() {
        [vbs] => ("", *vbs),
        [services, vbs] => (*services, *vbs),
        _ => {
            return Err(format!(
                "expected 1 or 2 lines from Win32_DeviceGuard, got: {output:?}"
            ));
        }
    };
    let security_services_running = services
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u32>()
                .map_err(|e| format!("bad SecurityServicesRunning value {s:?}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let virtualization_based_security_status = vbs
        .parse::<u32>()
        .map_err(|e| format!("bad VirtualizationBasedSecurityStatus {vbs:?}: {e}"))?;
    Ok(DeviceGuard {
        security_services_running,
        virtualization_based_security_status,
    })
}

/// Works out the label from the Device Guard state, and refuses a run unless HVCI runs
/// (both configurations need it) and kernel stack protection is either enforced or off.
///
/// With `expected`, the run also refuses when the system does not match it.
pub(crate) fn resolve_label(expected: Option<Label>, dg: &DeviceGuard) -> Result<Label, String> {
    let running = |v| dg.security_services_running.contains(&v);
    if dg.virtualization_based_security_status != VBS_RUNNING {
        return Err(format!(
            "virtualization-based security is not running (VirtualizationBasedSecurityStatus = {}, need {VBS_RUNNING})",
            dg.virtualization_based_security_status
        ));
    }
    if !running(HVCI) {
        return Err(format!(
            "memory integrity (HVCI, {HVCI}) is not running. Both configurations need it, so that kernel stack protection is the only difference"
        ));
    }
    if running(KERNEL_STACK_PROTECTION_AUDIT) {
        return Err(format!(
            "kernel stack protection is in audit mode ({KERNEL_STACK_PROTECTION_AUDIT}): shadow stacks are active, so the run fits neither label"
        ));
    }
    let actual = if running(KERNEL_STACK_PROTECTION) {
        Label::On
    } else {
        Label::Off
    };
    match expected {
        Some(expected) if expected != actual => Err(format!(
            "--label is \"{expected}\" but kernel stack protection ({KERNEL_STACK_PROTECTION}) is {actual}"
        )),
        _ => Ok(actual),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dg(services: &[u32]) -> DeviceGuard {
        DeviceGuard {
            security_services_running: services.to_vec(),
            virtualization_based_security_status: VBS_RUNNING,
        }
    }

    #[test]
    fn parses_list_and_status() {
        assert_eq!(parse("2,5\r\n2\r\n").unwrap(), dg(&[2, 5]));
        assert_eq!(parse("1, 2 ,5\n2").unwrap(), dg(&[1, 2, 5]));
    }

    #[test]
    fn parses_empty_service_list() {
        let want = DeviceGuard {
            security_services_running: vec![],
            virtualization_based_security_status: 0,
        };
        assert_eq!(parse("\r\n0\r\n").unwrap(), want);
        assert_eq!(parse("0\r\n").unwrap(), want);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("").is_err());
        assert!(parse("2,x\n2").is_err());
        assert!(parse("2\n2\n2").is_err());
        assert!(parse("2\nrunning").is_err());
    }

    #[test]
    fn label_follows_kernel_stack_protection() {
        assert_eq!(resolve_label(None, &dg(&[2, 5])), Ok(Label::On));
        assert_eq!(resolve_label(None, &dg(&[2])), Ok(Label::Off));
        assert_eq!(resolve_label(None, &dg(&[1, 2, 7])), Ok(Label::Off));
    }

    #[test]
    fn expected_label_must_match() {
        assert_eq!(resolve_label(Some(Label::On), &dg(&[2, 5])), Ok(Label::On));
        assert_eq!(resolve_label(Some(Label::Off), &dg(&[2])), Ok(Label::Off));
        assert!(resolve_label(Some(Label::On), &dg(&[2])).is_err());
        assert!(resolve_label(Some(Label::Off), &dg(&[2, 5])).is_err());
    }

    #[test]
    fn hvci_is_required() {
        assert!(resolve_label(None, &dg(&[5])).is_err());
        assert!(resolve_label(None, &dg(&[])).is_err());
    }

    #[test]
    fn audit_mode_and_stopped_vbs_are_refused() {
        assert!(resolve_label(None, &dg(&[2, 6])).is_err());
        assert!(resolve_label(None, &dg(&[2, 5, 6])).is_err());
        let mut d = dg(&[2, 5]);
        d.virtualization_based_security_status = 1;
        assert!(resolve_label(None, &d).is_err());
    }
}
