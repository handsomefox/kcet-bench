//! `compare`: groups run files by label and decides whether each delta clears the noise.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::report::{Label, Outcome, RunReport, SCHEMA_VERSION};
use crate::stats;

/// Runs whose implied boot times are this close belong to the same boot. Boot time is
/// derived from wall clock minus uptime, so clock adjustments make it drift a little.
const SAME_BOOT_MS: u64 = 120_000;
/// The run protocol asks for at least this many boots per label.
pub(crate) const MIN_BOOTS: usize = 3;

/// One label's results for one test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LabelStats {
    pub(crate) runs: usize,
    /// Median of the run medians.
    pub(crate) median: f64,
    /// Smallest and largest run median.
    pub(crate) lo: f64,
    pub(crate) hi: f64,
}

impl LabelStats {
    pub(crate) fn from_run_medians(medians: &[f64]) -> Option<Self> {
        let median = stats::median(medians)?;
        let lo = medians.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = medians.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some(Self {
            runs: medians.len(),
            median,
            lo,
            hi,
        })
    }

    pub(crate) fn spread(&self) -> f64 {
        self.hi - self.lo
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The difference of medians is at least the larger run-to-run spread.
    AboveNoise,
    WithinNoise,
    /// A spread needs two runs; with one, the noise rule cannot be applied.
    TooFewRuns,
}

/// Compares the absolute difference of the two medians with the larger of the two
/// spreads, both in nanoseconds. A difference smaller than the spread is within noise.
pub(crate) fn verdict(off: &LabelStats, on: &LabelStats) -> Verdict {
    if off.runs < 2 || on.runs < 2 {
        return Verdict::TooFewRuns;
    }
    let noise = off.spread().max(on.spread());
    if (on.median - off.median).abs() < noise {
        Verdict::WithinNoise
    } else {
        Verdict::AboveNoise
    }
}

/// Change from `off` to `on`, in percent of `off`.
pub(crate) fn delta_percent(off: &LabelStats, on: &LabelStats) -> f64 {
    (on.median - off.median) / off.median * 100.0
}

/// Groups boot times (Unix ms) into boots, oldest first. Times closer than
/// `SAME_BOOT_MS` to the previous time belong to the same boot.
pub(crate) fn boot_clusters(boot_times: &[u64]) -> Vec<Vec<u64>> {
    let mut sorted = boot_times.to_vec();
    sorted.sort_unstable();
    let mut clusters: Vec<Vec<u64>> = Vec::new();
    for t in sorted {
        match clusters.last_mut() {
            Some(cluster) if cluster.last().is_some_and(|&l| t - l <= SAME_BOOT_MS) => {
                cluster.push(t);
            }
            _ => clusters.push(vec![t]),
        }
    }
    clusters
}

/// Whether two boot times (Unix ms) belong to the same boot.
pub(crate) fn same_boot(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= SAME_BOOT_MS
}

/// Number of distinct boots among the given boot times (Unix ms).
pub(crate) fn count_boots(boot_times: &[u64]) -> usize {
    boot_clusters(boot_times).len()
}

/// A run file that has been loaded.
pub(crate) struct Loaded {
    pub(crate) path: String,
    pub(crate) report: RunReport,
}

pub(crate) fn group(runs: &[Loaded]) -> BTreeMap<Label, Vec<&Loaded>> {
    let mut groups: BTreeMap<Label, Vec<&Loaded>> = BTreeMap::new();
    for run in runs {
        groups.entry(run.report.label).or_default().push(run);
    }
    groups
}

/// Test names in the order they first appear across the runs.
fn test_names(runs: &[Loaded]) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = Vec::new();
    for run in runs {
        for t in &run.report.tests {
            if !names.iter().any(|(n, _)| *n == t.name) {
                let unit = match &t.outcome {
                    Outcome::Ok { unit, .. } => unit.clone(),
                    Outcome::Aborted { .. } => String::new(),
                };
                names.push((t.name.clone(), unit));
            } else if let Outcome::Ok { unit, .. } = &t.outcome
                && let Some(entry) = names.iter_mut().find(|(n, u)| *n == t.name && u.is_empty())
            {
                entry.1.clone_from(unit);
            }
        }
    }
    names
}

/// Collects the run medians and the number of aborted runs of one test in one group.
fn run_medians(runs: &[&Loaded], test: &str) -> (Vec<f64>, usize) {
    let mut medians = Vec::new();
    let mut aborted = 0;
    for run in runs {
        if let Some(t) = run.report.tests.iter().find(|t| t.name == test) {
            match t.median_ns() {
                Some(m) => medians.push(m),
                None => aborted += 1,
            }
        }
    }
    (medians, aborted)
}

/// Warns about settings that should be equal in every run but are not.
fn consistency_warnings(runs: &[Loaded]) -> Vec<String> {
    fn distinct<T: PartialEq + ToString>(
        runs: &[Loaded],
        get: impl Fn(&RunReport) -> T,
    ) -> Option<String> {
        let mut seen: Vec<T> = Vec::new();
        for run in runs {
            let v = get(&run.report);
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        (seen.len() > 1).then(|| {
            seen.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
    let checks: [(&str, Option<String>); 5] = [
        (
            "Windows builds",
            distinct(runs, |r| r.system.windows.to_string()),
        ),
        ("CPUs", distinct(runs, |r| r.system.cpu_brand.clone())),
        (
            "power schemes",
            distinct(runs, |r| {
                let p = &r.system.power_scheme;
                p.name.clone().unwrap_or_else(|| p.guid.clone())
            }),
        ),
        ("pinned CPUs", distinct(runs, |r| r.config.cpu)),
        ("tool versions", distinct(runs, |r| r.tool_version.clone())),
    ];
    checks
        .into_iter()
        .filter_map(|(what, found)| found.map(|f| format!("runs mix {what}: {f}")))
        .collect()
}

pub(crate) fn check_schema(run: &Loaded) -> Result<(), String> {
    if run.report.schema_version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(format!(
            "{}: schema version {} is not supported (expected {SCHEMA_VERSION})",
            run.path, run.report.schema_version
        ))
    }
}

/// Renders the comparison table and the notes under it.
pub(crate) fn render(runs: &[Loaded]) -> String {
    let groups = group(runs);
    let mut out = String::new();

    for (label, members) in &groups {
        let boots = count_boots(
            &members
                .iter()
                .map(|r| r.report.system.boot_time_ms())
                .collect::<Vec<_>>(),
        );
        let _ = writeln!(
            out,
            "{label}: {} run(s) over {boots} boot(s)",
            members.len()
        );
    }
    out.push('\n');

    let empty = Vec::new();
    let off = groups.get(&Label::Off).unwrap_or(&empty);
    let on = groups.get(&Label::On).unwrap_or(&empty);

    let _ = writeln!(
        out,
        "{:<20} {:<7} {:>12} {:>24} {:>12} {:>24} {:>8}  verdict",
        "test", "unit", "off median", "off spread (runs)", "on median", "on spread (runs)", "delta"
    );
    let mut notes = Vec::new();
    for (name, unit) in test_names(runs) {
        let (off_m, off_aborted) = run_medians(off, &name);
        let (on_m, on_aborted) = run_medians(on, &name);
        for (label, n) in [(Label::Off, off_aborted), (Label::On, on_aborted)] {
            if n > 0 {
                notes.push(format!("{name}: {n} {label} run(s) aborted and left out"));
            }
        }
        let off_s = LabelStats::from_run_medians(&off_m);
        let on_s = LabelStats::from_run_medians(&on_m);
        let cell = |s: Option<LabelStats>| match s {
            Some(s) => (
                format!("{:.1} ns", s.median),
                format!("{:.1}-{:.1} ({})", s.lo, s.hi, s.runs),
            ),
            None => ("-".to_owned(), "-".to_owned()),
        };
        let (off_med, off_spread) = cell(off_s);
        let (on_med, on_spread) = cell(on_s);
        let (delta, verdict_text) = match (off_s, on_s) {
            (Some(off_s), Some(on_s)) => (
                format!("{:+.1}%", delta_percent(&off_s, &on_s)),
                match verdict(&off_s, &on_s) {
                    Verdict::AboveNoise => "above noise",
                    Verdict::WithinNoise => "within noise",
                    Verdict::TooFewRuns => "need 2+ runs per label",
                },
            ),
            _ => ("-".to_owned(), "missing a label"),
        };
        let _ = writeln!(
            out,
            "{name:<20} {unit:<7} {off_med:>12} {off_spread:>24} {on_med:>12} {on_spread:>24} {delta:>8}  {verdict_text}"
        );
    }

    for (label, members) in &groups {
        let boots = count_boots(
            &members
                .iter()
                .map(|r| r.report.system.boot_time_ms())
                .collect::<Vec<_>>(),
        );
        if boots < MIN_BOOTS {
            notes.push(format!(
                "{label}: only {boots} boot(s); the protocol asks for at least {MIN_BOOTS}"
            ));
        }
    }
    notes.extend(consistency_warnings(runs));
    if !notes.is_empty() {
        out.push('\n');
        for n in notes {
            let _ = writeln!(out, "note: {n}");
        }
    }
    out.push_str(
        "\nspread = min-max of run medians within a label. \"within noise\" means |on - off| is smaller than the larger spread.\n",
    );
    out
}

/// A run file with fixed system details, for tests.
#[cfg(test)]
pub(crate) fn fake_run(label: Label, boot_ms: u64, medians: &[(&str, Option<f64>)]) -> Loaded {
    use crate::guard::DeviceGuard;
    use crate::report::{PowerScheme, RunConfig, SystemInfo, TestResult, WindowsVersion};
    use crate::stats::Summary;

    let tests = medians
        .iter()
        .map(|&(name, m)| TestResult {
            name: name.into(),
            outcome: match m {
                Some(m) => Outcome::Ok {
                    unit: "call".into(),
                    ops_per_batch: 1000,
                    median_batch_ms: 30.0,
                    batches_outside_window: 0,
                    ns_per_unit: Summary {
                        median: m,
                        p5: m,
                        p95: m,
                        min: m,
                    },
                    batch_ns_per_unit: vec![m],
                },
                None => Outcome::Aborted { reason: "x".into() },
            },
        })
        .collect();
    Loaded {
        path: format!("{label}.json"),
        report: RunReport {
            schema_version: SCHEMA_VERSION,
            tool_version: "0.1.0".into(),
            label,
            system: SystemInfo {
                timestamp: String::new(),
                unix_time_ms: boot_ms + 600_000,
                uptime_ms: 600_000,
                windows: WindowsVersion {
                    major: 10,
                    minor: 0,
                    build: 26100,
                    ubr: Some(1),
                },
                cpu_brand: "cpu".into(),
                cpu_shadow_stack_support: true,
                logical_cpus: 8,
                power_scheme: PowerScheme::from_guid("381b4222-f694-41f0-9685-ff5bb260df2e".into()),
                device_guard: DeviceGuard {
                    security_services_running: vec![2],
                    virtualization_based_security_status: 2,
                },
                user_shadow_stack: false,
            },
            config: RunConfig {
                cpu: 2,
                qpc_frequency: 10_000_000,
                warmup_batches: 5,
                batches: 50,
                batch_min_ms: 20,
                batch_target_ms: 35,
                batch_max_ms: 50,
            },
            tests,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(medians: &[f64]) -> LabelStats {
        LabelStats::from_run_medians(medians).unwrap()
    }

    #[test]
    fn label_stats_take_median_and_range_of_run_medians() {
        let s = stats(&[110.0, 100.0, 104.0, 120.0]);
        assert_eq!((s.runs, s.median, s.lo, s.hi), (4, 107.0, 100.0, 120.0));
        assert!((s.spread() - 20.0).abs() < f64::EPSILON);
        assert_eq!(LabelStats::from_run_medians(&[]), None);
    }

    #[test]
    fn noise_rule_uses_larger_spread() {
        let off = stats(&[100.0, 101.0, 102.0]); // spread 2
        let on_wide = stats(&[100.0, 104.0, 110.0]); // spread 10, median 104
        assert_eq!(verdict(&off, &on_wide), Verdict::WithinNoise);
        let on_tight = stats(&[104.0, 105.0, 106.0]); // spread 2, delta 4
        assert_eq!(verdict(&off, &on_tight), Verdict::AboveNoise);
        assert!((delta_percent(&off, &on_tight) - 3.960_396).abs() < 1e-5);
    }

    #[test]
    fn delta_equal_to_spread_is_above_noise() {
        let off = stats(&[100.0, 102.0]); // median 101, spread 2
        let on = stats(&[102.0, 104.0]); // median 103
        assert_eq!(verdict(&off, &on), Verdict::AboveNoise);
    }

    #[test]
    fn noise_rule_needs_two_runs_per_label() {
        assert_eq!(
            verdict(&stats(&[100.0]), &stats(&[200.0, 201.0])),
            Verdict::TooFewRuns
        );
    }

    #[test]
    fn counts_boots_by_clustering_boot_times() {
        assert_eq!(count_boots(&[]), 0);
        assert_eq!(count_boots(&[1_000_000, 1_030_000, 1_010_000]), 1);
        assert_eq!(
            count_boots(&[1_000_000, 9_000_000, 1_050_000, 9_001_000, 20_000_000]),
            3
        );
    }

    #[test]
    fn run_file_round_trips_through_json() {
        let original = fake_run(Label::On, 5_000, &[("a", Some(123.25)), ("b", None)]).report;
        let json = serde_json::to_string_pretty(&original).unwrap();
        let parsed: RunReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.label, Label::On);
        assert_eq!(parsed.system.boot_time_ms(), 5_000);
        assert_eq!(parsed.tests[0].median_ns(), Some(123.25));
        assert!(matches!(parsed.tests[1].outcome, Outcome::Aborted { .. }));
        assert!(json.contains(r#""status": "ok""#), "{json}");
        assert!(json.contains(r#""label": "on""#), "{json}");
    }

    #[test]
    fn groups_by_label() {
        let runs = [
            fake_run(Label::On, 0, &[]),
            fake_run(Label::Off, 0, &[]),
            fake_run(Label::On, 0, &[]),
        ];
        let g = group(&runs);
        assert_eq!(g[&Label::On].len(), 2);
        assert_eq!(g[&Label::Off].len(), 1);
    }

    #[test]
    fn render_skips_aborted_runs_and_flags_noise() {
        let runs = [
            fake_run(Label::Off, 0, &[("a", Some(100.0)), ("b", Some(50.0))]),
            fake_run(Label::Off, 10_000_000, &[("a", Some(101.0)), ("b", None)]),
            fake_run(
                Label::On,
                20_000_000,
                &[("a", Some(110.0)), ("b", Some(50.5))],
            ),
            fake_run(
                Label::On,
                30_000_000,
                &[("a", Some(111.0)), ("b", Some(51.0))],
            ),
        ];
        let text = render(&runs);
        let line = |name: &str| {
            text.lines()
                .find(|l| l.starts_with(&format!("{name} ")))
                .unwrap()
                .to_owned()
        };
        assert!(line("a").ends_with("above noise"), "{text}");
        assert!(line("a").contains("+10.0%"), "{text}");
        // b has one valid off run, so the noise rule cannot apply.
        assert!(line("b").ends_with("need 2+ runs per label"), "{text}");
        assert!(text.contains("b: 1 off run(s) aborted"), "{text}");
        assert!(text.contains("off: only 2 boot(s)"), "{text}");
    }

    #[test]
    fn warns_about_mixed_settings() {
        let mut other = fake_run(Label::On, 0, &[]);
        other.report.config.cpu = 4;
        other.report.system.windows.ubr = Some(2);
        let runs = [fake_run(Label::Off, 0, &[]), other];
        let warnings = consistency_warnings(&runs);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("10.0.26100.1, 10.0.26100.2"));
    }
}
