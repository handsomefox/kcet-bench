//! `run`: everything that only works on Windows.

mod bench;
mod sys;
mod sysinfo;

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::process::ExitCode;

use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
    SetProcessAffinityMask, SetProcessInformation, SetThreadAffinityMask,
};

use crate::cli::RunArgs;
use crate::eprint_line;
use crate::guard;
use crate::report::{Outcome, RunConfig, RunReport, SCHEMA_VERSION, SystemInfo, TestResult};
use crate::stats;
use crate::tune::{self, Plan};
use bench::{Bench, Spec};
use sys::{last_error, qpc, qpc_frequency, size_u32};

/// The run protocol asks for 3 idle minutes after boot before the first run.
const SETTLE_MS: u64 = 180_000;

pub(crate) fn run(args: &RunArgs) -> Result<ExitCode, String> {
    // Create the output first, so a bad path fails before minutes of measuring.
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.out)
        .map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;
    let result = measure_all(args, &mut out);
    if result.is_err() {
        drop(out);
        let _ = std::fs::remove_file(&args.out);
    }
    result
}

fn measure_all(args: &RunArgs, out: &mut File) -> Result<ExitCode, String> {
    // Query Device Guard before raising priority and pinning: PowerShell inherits the
    // process affinity mask, and it should not compete with the measurements later.
    let device_guard = sysinfo::device_guard()?;
    guard::check_label(args.label, &device_guard)?;
    let user_shadow_stack = sysinfo::user_shadow_stack()?;
    if user_shadow_stack {
        return Err("user-mode shadow stacks are on for this process (forced by policy?). They would add a user-mode cost, so only kernel stack protection may differ".into());
    }

    let now = sysinfo::now();
    if now.uptime_ms < SETTLE_MS {
        eprint_line(&format!(
            "warning: the system booted {} s ago; the protocol asks for 3 idle minutes first",
            now.uptime_ms / 1000
        ));
    }
    let system = SystemInfo {
        timestamp: now.timestamp,
        unix_time_ms: now.unix_time_ms,
        uptime_ms: now.uptime_ms,
        windows: sysinfo::windows_version()?,
        cpu_brand: sysinfo::cpu_brand(),
        cpu_shadow_stack_support: sysinfo::cpu_shadow_stack_support(),
        logical_cpus: sysinfo::logical_cpus(),
        power_scheme: sysinfo::power_scheme()?,
        device_guard,
        user_shadow_stack,
    };

    prepare_process(args.cpu)?;
    let plan = Plan::standard(qpc_frequency()?);
    eprint_line(&format!(
        "kcet-bench {}: label {}, CPU {}, Windows {}, {}",
        env!("CARGO_PKG_VERSION"),
        args.label,
        args.cpu,
        system.windows,
        system.cpu_brand
    ));

    let tests: Vec<TestResult> = bench::ALL
        .iter()
        .map(|spec| run_test(spec, &plan))
        .collect();
    let aborted = tests
        .iter()
        .filter(|t| matches!(t.outcome, Outcome::Aborted { .. }))
        .count();

    let report = RunReport {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        label: args.label,
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
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    out.write_all(json.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", args.out.display()))?;
    eprint_line(&format!("wrote {}", args.out.display()));

    if aborted > 0 {
        eprint_line(&format!(
            "error: {aborted} test(s) aborted; their numbers are not in the file"
        ));
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
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
