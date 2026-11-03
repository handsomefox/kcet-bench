//! `run`: everything that only works on Windows.

mod bench;
mod sys;
mod sysinfo;

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
    SetProcessAffinityMask, SetProcessInformation, SetThreadAffinityMask,
};

use crate::cli::RunArgs;
use crate::guard;
use crate::report::{Label, Outcome, RunConfig, RunReport, SCHEMA_VERSION, SystemInfo, TestResult};
use crate::session::{self, Slot};
use crate::stats;
use crate::tune::{self, Plan};
use crate::{compare, eprint_line, print_text};
use bench::{Bench, Spec};
use sys::{last_error, qpc, qpc_frequency, size_u32};

/// The run protocol asks for 3 idle minutes after boot before the first run.
const SETTLE_MS: u64 = 180_000;

pub(crate) fn run(args: &RunArgs) -> Result<ExitCode, String> {
    // Query Device Guard before raising priority and pinning: PowerShell inherits the
    // process affinity mask, and it should not compete with the measurements later.
    let device_guard = sysinfo::device_guard()?;
    let label = guard::resolve_label(args.label, &device_guard)?;
    let user_shadow_stack = sysinfo::user_shadow_stack()?;
    if user_shadow_stack {
        return Err("user-mode shadow stacks are on for this process (forced by policy?). They would add a user-mode cost, so only kernel stack protection may differ".into());
    }
    std::fs::create_dir_all(&args.dir)
        .map_err(|e| format!("cannot create {}: {e}", args.dir.display()))?;

    let (earlier, problems) = session::load_dir_lenient(&args.dir);
    for problem in problems {
        eprint_line(&format!("warning: skipping {problem}"));
    }
    let start = sysinfo::now();
    let boot_ms = start.unix_time_ms.saturating_sub(start.uptime_ms);
    let slot = session::slot(&earlier, label, boot_ms);
    warn_and_settle(&earlier, label, boot_ms, start.uptime_ms);

    let fixed = FixedInfo {
        windows: sysinfo::windows_version()?,
        cpu_brand: sysinfo::cpu_brand(),
        cpu_shadow_stack_support: sysinfo::cpu_shadow_stack_support(),
        logical_cpus: sysinfo::logical_cpus(),
        power_scheme: sysinfo::power_scheme()?,
    };
    prepare_process(args.cpu)?;
    let plan = Plan::standard(qpc_frequency()?);
    eprint_line(&format!(
        "kcet-bench {}: label {label} (detected), boot {}, CPU {}, Windows {}, {}",
        env!("CARGO_PKG_VERSION"),
        slot.boot,
        args.cpu,
        fixed.windows,
        fixed.cpu_brand
    ));

    let mut aborted = 0;
    for i in 0..args.runs {
        eprint_line(&format!("run {} of {}", i + 1, args.runs));
        let now = sysinfo::now();
        let system = SystemInfo {
            timestamp: now.timestamp,
            unix_time_ms: now.unix_time_ms,
            uptime_ms: now.uptime_ms,
            windows: fixed.windows.clone(),
            cpu_brand: fixed.cpu_brand.clone(),
            cpu_shadow_stack_support: fixed.cpu_shadow_stack_support,
            logical_cpus: fixed.logical_cpus,
            power_scheme: fixed.power_scheme.clone(),
            device_guard: device_guard.clone(),
            user_shadow_stack,
        };
        let tests: Vec<TestResult> = bench::ALL
            .iter()
            .map(|spec| run_test(spec, &plan))
            .collect();
        aborted += tests
            .iter()
            .filter(|t| matches!(t.outcome, Outcome::Aborted { .. }))
            .count();
        let report = RunReport {
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            label,
            system,
            config: RunConfig {
                cpu: args.cpu,
                qpc_frequency: plan.freq,
                warmup_batches: plan.warmup_batches,
                batches: plan.batches,
                batch_min_ms: plan.min_ms,
                batch_target_ms: plan.target_ms,
                batch_max_ms: plan.max_ms,
            },
            tests,
        };
        let path = write_new(&args.dir, label, &slot, i, &report)?;
        eprint_line(&format!("wrote {}", path.display()));
    }

    let (all, _) = session::load_dir_lenient(&args.dir);
    print_text(&format!(
        "\n{}\n{}",
        compare::render(&all),
        session::next_step(&all, label)
    ));
    if aborted > 0 {
        eprint_line(&format!(
            "error: {aborted} test run(s) aborted; their numbers are not in the files"
        ));
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Warns about protocol slips the tool can see, and waits out the 3 idle minutes after boot.
fn warn_and_settle(earlier: &[compare::Loaded], label: Label, boot_ms: u64, uptime_ms: u64) {
    if session::previous_boot_label(earlier, boot_ms) == Some(label) {
        eprint_line(&format!(
            "warning: the previous boot was also {label}; the protocol alternates off and on"
        ));
    }
    let vms = sysinfo::running_vm_processes();
    if !vms.is_empty() {
        eprint_line(&format!(
            "warning: a virtual machine is running ({}). Run `wsl --shutdown` and stop other VMs first",
            vms.join(", ")
        ));
    }
    if uptime_ms < SETTLE_MS {
        let wait = SETTLE_MS - uptime_ms;
        eprint_line(&format!(
            "waiting {} s, so the system has been up for 3 minutes before measuring",
            wait.div_ceil(1000)
        ));
        std::thread::sleep(Duration::from_millis(wait));
    }
}

/// System details that do not change between the runs of one invocation.
struct FixedInfo {
    windows: crate::report::WindowsVersion,
    cpu_brand: String,
    cpu_shadow_stack_support: bool,
    logical_cpus: u32,
    power_scheme: crate::report::PowerScheme,
}

/// Writes `report` to the first free `<label>-boot<n>-run<n>.json` name for this run.
/// An existing file is never overwritten.
fn write_new(
    dir: &Path,
    label: Label,
    slot: &Slot,
    index: usize,
    report: &RunReport,
) -> Result<PathBuf, String> {
    let json = serde_json::to_string_pretty(report).map_err(|e| e.to_string())? + "\n";
    let mut run = slot.first_run + index;
    loop {
        let path = dir.join(Slot::file_name(label, slot.boot, run));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(json.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => run += 1,
            Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
        }
    }
}

/// Raises the priority class, opts out of power throttling, and pins the process (and with
/// it every thread it creates later) and the current thread to `cpu`.
fn prepare_process(cpu: u32) -> Result<(), String> {
    // SAFETY: GetActiveProcessorCount has no preconditions. Group 0 is the group that
    // SetProcessAffinityMask works in.
    let in_group = unsafe { GetActiveProcessorCount(0) };
    if cpu == 0 || cpu >= in_group.min(usize::BITS) {
        return Err(format!(
            "--cpu {cpu} is out of range: use 1..{}",
            in_group.min(usize::BITS) - 1
        ));
    }
    // SAFETY: GetCurrentProcess has no preconditions and returns a pseudo-handle.
    let process = unsafe { GetCurrentProcess() };

    // SAFETY: `process` is the current-process pseudo-handle.
    if unsafe { SetPriorityClass(process, HIGH_PRIORITY_CLASS) } == 0 {
        return Err(last_error("SetPriorityClass(HIGH_PRIORITY_CLASS)"));
    }

    // Opt out of EcoQoS so Windows does not lower the clock speed it picks for this process.
    let throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    // SAFETY: `throttling` is a valid PROCESS_POWER_THROTTLING_STATE of the given size.
    let ok = unsafe {
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw const throttling).cast::<c_void>(),
            size_u32::<PROCESS_POWER_THROTTLING_STATE>(),
        )
    };
    if ok == 0 {
        return Err(last_error("SetProcessInformation(ProcessPowerThrottling)"));
    }

    let mask = 1usize << cpu;
    // SAFETY: `process` is the current-process pseudo-handle and `mask` names an active CPU.
    if unsafe { SetProcessAffinityMask(process, mask) } == 0 {
        return Err(last_error("SetProcessAffinityMask"));
    }
    // SAFETY: GetCurrentThread has no preconditions and returns a pseudo-handle.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: `thread` is the current-thread pseudo-handle; `mask` is within the process mask.
    if unsafe { SetThreadAffinityMask(thread, mask) } == 0 {
        return Err(last_error("SetThreadAffinityMask"));
    }
    Ok(())
}

fn run_test(spec: &Spec, plan: &Plan) -> TestResult {
    let outcome = match measure(spec, plan) {
        Ok(outcome) => outcome,
        Err(reason) => {
            eprint_line(&format!("{:<20} aborted: {reason}", spec.name));
            Outcome::Aborted { reason }
        }
    };
    TestResult {
        name: spec.name.to_owned(),
        outcome,
    }
}

fn measure(spec: &Spec, plan: &Plan) -> Result<Outcome, String> {
    let mut bench = (spec.setup)()?;
    let measured = tune::measure(plan, |n| timed_batch(bench.as_mut(), n))?;
    drop(bench);

    let batch_ns_per_unit = measured.ns_per_unit(plan.freq, spec.units_per_op);
    let ns_per_unit = stats::summarize(&batch_ns_per_unit).ok_or("no valid batch timings")?;
    let batch_ms: Vec<f64> = measured
        .batch_ticks
        .iter()
        .map(|&t| tune::ticks_to_ms(t, plan.freq))
        .collect();
    let median_batch_ms = stats::median(&batch_ms).ok_or("no valid batch timings")?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the window bounds are small integers"
    )]
    let window = plan.min_ms as f64..=plan.max_ms as f64;
    let batches_outside_window = batch_ms.iter().filter(|ms| !window.contains(ms)).count();

    eprint_line(&format!(
        "{:<20} {:>10.1} ns/{} (p5 {:.1}, p95 {:.1}, min {:.1}), {} ops/batch",
        spec.name,
        ns_per_unit.median,
        spec.unit,
        ns_per_unit.p5,
        ns_per_unit.p95,
        ns_per_unit.min,
        measured.ops_per_batch
    ));
    Ok(Outcome::Ok {
        unit: spec.unit.to_owned(),
        ops_per_batch: measured.ops_per_batch,
        median_batch_ms,
        batches_outside_window,
        ns_per_unit,
        batch_ns_per_unit,
    })
}

/// Times one batch with the performance counter.
fn timed_batch(bench: &mut dyn Bench, n: u64) -> Result<u64, String> {
    let start = qpc()?;
    bench.batch(n)?;
    let end = qpc()?;
    u64::try_from(end - start)
        .map_err(|_| format!("performance counter went backwards ({start} -> {end})"))
}
