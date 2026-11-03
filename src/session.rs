//! The results folder: loading run files, naming new ones, and the next protocol step.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::compare::{self, Loaded, MIN_BOOTS};
use crate::report::Label;

/// Where `run` writes and `compare` reads when no folder is given.
pub(crate) const DEFAULT_DIR: &str = "results";

/// Loads run files. A folder stands for every `.json` file directly inside it.
/// Any file that cannot be read or parsed is an error.
pub(crate) fn load(paths: &[PathBuf]) -> Result<Vec<Loaded>, String> {
    let mut runs = Vec::new();
    for path in paths {
        if path.is_dir() {
            for file in json_files(path)? {
                runs.push(load_file(&file)?);
            }
        } else {
            runs.push(load_file(path)?);
        }
    }
    Ok(runs)
}

/// Loads every run file in `dir` that parses, and describes the ones that do not.
/// A missing folder holds no runs.
pub(crate) fn load_dir_lenient(dir: &Path) -> (Vec<Loaded>, Vec<String>) {
    if !dir.exists() {
        return (Vec::new(), Vec::new());
    }
    let files = match json_files(dir) {
        Ok(files) => files,
        Err(e) => return (Vec::new(), vec![e]),
    };
    let mut runs = Vec::new();
    let mut problems = Vec::new();
    for file in files {
        match load_file(&file) {
            Ok(run) => runs.push(run),
            Err(e) => problems.push(e),
        }
    }
    (runs, problems)
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn load_file(path: &Path) -> Result<Loaded, String> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{display}: {e}"))?;
    let report = serde_json::from_str(&text).map_err(|e| format!("{display}: {e}"))?;
    let loaded = Loaded {
        path: display,
        report,
    };
    compare::check_schema(&loaded)?;
    Ok(loaded)
}

/// Where the next run of this boot goes: the boot's number among this label's boots,
/// and the first run number after the runs this boot already has.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) boot: usize,
    pub(crate) first_run: usize,
}

impl Slot {
    pub(crate) fn file_name(label: Label, boot: usize, run: usize) -> String {
        format!("{label}-boot{boot}-run{run}.json")
    }
}

/// Numbers the current boot among the earlier boots of `label`, oldest first. A boot that
/// already has runs keeps its number, and its next run number follows them.
pub(crate) fn slot(runs: &[Loaded], label: Label, boot_ms: u64) -> Slot {
    let times: Vec<u64> = runs
        .iter()
        .filter(|r| r.report.label == label)
        .map(|r| r.report.system.boot_time_ms())
        .collect();
    let clusters = compare::boot_clusters(&times);
    match clusters
        .iter()
        .position(|c| c.iter().any(|&t| compare::same_boot(t, boot_ms)))
    {
        Some(i) => Slot {
            boot: i + 1,
            first_run: clusters[i].len() + 1,
        },
        None => Slot {
            boot: clusters.len() + 1,
            first_run: 1,
        },
    }
}

/// The label of the most recent earlier boot, ignoring runs from the current boot.
pub(crate) fn previous_boot_label(runs: &[Loaded], boot_ms: u64) -> Option<Label> {
    runs.iter()
        .map(|r| (r.report.system.boot_time_ms(), r.report.label))
        .filter(|&(t, _)| !compare::same_boot(t, boot_ms) && t < boot_ms)
        .max_by_key(|&(t, _)| t)
        .map(|(_, label)| label)
}

/// What the protocol asks for after a boot with `label`, given every run so far.
pub(crate) fn next_step(runs: &[Loaded], label: Label) -> String {
    let boots = |l: Label| {
        let times: Vec<u64> = runs
            .iter()
            .filter(|r| r.report.label == l)
            .map(|r| r.report.system.boot_time_ms())
            .collect();
        compare::count_boots(&times)
    };
    let (off, on) = (boots(Label::Off), boots(Label::On));
    let mut text = format!("Boots so far: off {off}, on {on}.\n");
    if off >= MIN_BOOTS && on >= MIN_BOOTS {
        text.push_str(
            "Each setting has at least 3 boots, so the comparison above is complete. More boots narrow the spread.\n",
        );
    } else {
        let next = match label {
            Label::On => Label::Off,
            Label::Off => Label::On,
        };
        let _ = writeln!(
            text,
            "Next: turn Kernel-mode Hardware-enforced Stack Protection {next} in Windows Security > Device security > Core isolation details, restart Windows, and run kcet-bench again."
        );
    }
    text
}

/// Whether a process name belongs to a running virtual machine: `vmmemWSL` for WSL2,
/// `vmmem` for other Hyper-V VMs and older WSL2 versions.
pub(crate) fn is_vm_process(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(stem, "vmmem" | "vmmemwsl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::fake_run;

    const HOUR: u64 = 3_600_000;

    #[test]
    fn first_boot_of_a_label_is_boot_1() {
        assert_eq!(
            slot(&[], Label::Off, 10 * HOUR),
            Slot {
                boot: 1,
                first_run: 1
            }
        );
        // Runs of the other label do not count.
        let runs = [fake_run(Label::On, HOUR, &[])];
        assert_eq!(slot(&runs, Label::Off, 10 * HOUR).boot, 1);
    }

    #[test]
    fn new_boot_follows_earlier_boots_of_the_same_label() {
        let runs = [
            fake_run(Label::Off, HOUR, &[]),
            fake_run(Label::Off, HOUR, &[]),
            fake_run(Label::On, 2 * HOUR, &[]),
            fake_run(Label::Off, 3 * HOUR, &[]),
        ];
        assert_eq!(
            slot(&runs, Label::Off, 5 * HOUR),
            Slot {
                boot: 3,
                first_run: 1
            }
        );
        assert_eq!(
            slot(&runs, Label::On, 5 * HOUR),
            Slot {
                boot: 2,
                first_run: 1
            }
        );
    }

    #[test]
    fn same_boot_continues_its_run_numbers() {
        // Boot times drift by a few seconds when the clock is adjusted.
        let runs = [
            fake_run(Label::Off, HOUR, &[]),
            fake_run(Label::Off, 3 * HOUR, &[]),
            fake_run(Label::Off, 3 * HOUR + 2_000, &[]),
        ];
        assert_eq!(
            slot(&runs, Label::Off, 3 * HOUR + 5_000),
            Slot {
                boot: 2,
                first_run: 3
            }
        );
    }

    #[test]
    fn recognizes_vm_memory_processes() {
        for name in ["vmmem", "vmmemWSL", "VMMEMWSL.EXE"] {
            assert!(is_vm_process(name), "{name}");
        }
        for name in ["vmmemory", "wsl.exe", "explorer.exe", ""] {
            assert!(!is_vm_process(name), "{name}");
        }
    }

    #[test]
    fn names_files_by_label_boot_and_run() {
        assert_eq!(Slot::file_name(Label::On, 2, 3), "on-boot2-run3.json");
    }

    #[test]
    fn previous_boot_skips_the_current_one() {
        let runs = [
            fake_run(Label::Off, HOUR, &[]),
            fake_run(Label::On, 2 * HOUR, &[]),
            fake_run(Label::Off, 3 * HOUR, &[]),
        ];
        assert_eq!(
            previous_boot_label(&runs, 3 * HOUR + 1_000),
            Some(Label::On)
        );
        assert_eq!(previous_boot_label(&runs, 4 * HOUR), Some(Label::Off));
        assert_eq!(previous_boot_label(&[], 4 * HOUR), None);
    }

    #[test]
    fn next_step_alternates_until_each_label_has_three_boots() {
        let mut runs = vec![fake_run(Label::Off, HOUR, &[])];
        let text = next_step(&runs, Label::Off);
        assert!(text.contains("off 1, on 0"), "{text}");
        assert!(text.contains("Stack Protection on"), "{text}");
        for i in 2..=6 {
            let label = if i % 2 == 0 { Label::On } else { Label::Off };
            runs.push(fake_run(label, i * HOUR, &[]));
        }
        let text = next_step(&runs, Label::On);
        assert!(text.contains("off 3, on 3"), "{text}");
        assert!(text.contains("complete"), "{text}");
    }

    #[test]
    fn loads_folders_and_reports_bad_files() {
        let dir = std::env::temp_dir().join(format!("kcet-bench-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = serde_json::to_string(&fake_run(Label::On, HOUR, &[]).report).unwrap();
        std::fs::write(dir.join("on-boot1-run1.json"), good).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a run").unwrap();
        assert_eq!(load(std::slice::from_ref(&dir)).unwrap().len(), 1);

        std::fs::write(dir.join("broken.json"), "{").unwrap();
        assert!(load(std::slice::from_ref(&dir)).is_err());
        let (runs, problems) = load_dir_lenient(&dir);
        assert_eq!((runs.len(), problems.len()), (1, 1));
        assert!(problems[0].contains("broken.json"), "{problems:?}");

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(load_dir_lenient(&dir).0.len(), 0);
    }
}
