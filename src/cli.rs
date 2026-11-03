//! Command-line parsing. No framework: there are two commands and four flags.

use std::path::PathBuf;

use crate::report::Label;
use crate::session::DEFAULT_DIR;

/// The logical CPU measuring threads are pinned to unless `--cpu` says otherwise.
pub(crate) const DEFAULT_CPU: u32 = 2;
/// Runs per boot unless `--runs` says otherwise. The run protocol asks for 3.
pub(crate) const DEFAULT_RUNS: usize = 3;
const MAX_RUNS: usize = 20;

pub(crate) const USAGE: &str = "\
kcet-bench: measures what Windows kernel-mode hardware-enforced stack protection costs

Usage:
  kcet-bench run [--runs <n>] [--dir <folder>] [--cpu <n>] [--label <on|off>]
  kcet-bench compare [<file or folder>...]
  kcet-bench --help | --version

run      Measure every test 3 times and write each run to its own JSON file, named
         <label>-boot<n>-run<n>.json. The label comes from the Device Guard state,
         and the boot and run numbers continue from the files already in the folder.
         Afterwards, print the comparison so far and the next step of the protocol.
         --runs   Runs in this boot (default 3).
         --dir    Results folder (default results). It is created when missing.
         --cpu    Logical CPU to pin every measuring thread to (default 2, never 0).
         --label  Refuse to run unless the system has this setting.
compare  Group run files by label and report each test's delta and noise. A folder
         stands for every .json file in it (default results).
";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    pub(crate) label: Option<Label>,
    pub(crate) dir: PathBuf,
    pub(crate) runs: usize,
    pub(crate) cpu: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Run(RunArgs),
    Compare(Vec<PathBuf>),
    Help,
    Version,
}

pub(crate) fn parse(args: &[String]) -> Result<Command, String> {
    let Some((command, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    match command.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        "run" => parse_run(rest).map(Command::Run),
        "compare" => {
            if let Some(flag) = rest.iter().find(|a| a.starts_with('-')) {
                return Err(format!("compare takes only files and folders, got {flag}"));
            }
            if rest.is_empty() {
                return Ok(Command::Compare(vec![PathBuf::from(DEFAULT_DIR)]));
            }
            Ok(Command::Compare(rest.iter().map(PathBuf::from).collect()))
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut label = None;
    let mut dir = None;
    let mut runs = None;
    let mut cpu = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--label" => {
                let v = value()?;
                let parsed = Label::parse(v)
                    .ok_or_else(|| format!("--label must be on or off, got {v:?}"))?;
                set_once(&mut label, parsed, flag)?;
            }
            "--dir" => set_once(&mut dir, PathBuf::from(value()?), flag)?,
            "--runs" => {
                let v = value()?;
                let n: usize = v
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_RUNS).contains(n))
                    .ok_or_else(|| format!("--runs must be 1 to {MAX_RUNS}, got {v:?}"))?;
                set_once(&mut runs, n, flag)?;
            }
            "--cpu" => {
                let v = value()?;
                let n: u32 = v
                    .parse()
                    .map_err(|_| format!("--cpu must be a CPU number, got {v:?}"))?;
                if n == 0 {
                    return Err(
                        "--cpu 0 is not allowed: CPU 0 services most interrupts and DPCs".into(),
                    );
                }
                set_once(&mut cpu, n, flag)?;
            }
            other => return Err(format!("unknown flag for run: {other:?}")),
        }
    }
    Ok(RunArgs {
        label,
        dir: dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DIR)),
        runs: runs.unwrap_or(DEFAULT_RUNS),
        cpu: cpu.unwrap_or(DEFAULT_CPU),
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} given twice"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn run_needs_no_flags() {
        assert_eq!(
            parse(&args("run")).unwrap(),
            Command::Run(RunArgs {
                label: None,
                dir: DEFAULT_DIR.into(),
                runs: DEFAULT_RUNS,
                cpu: DEFAULT_CPU,
            })
        );
    }

    #[test]
    fn parses_run_flags() {
        assert_eq!(
            parse(&args("run --cpu 6 --dir D:\\bench --runs 5 --label off")).unwrap(),
            Command::Run(RunArgs {
                label: Some(Label::Off),
                dir: "D:\\bench".into(),
                runs: 5,
                cpu: 6,
            })
        );
    }

    #[test]
    fn rejects_bad_run_flags() {
        for bad in [
            "run --label maybe",
            "run --cpu 0",
            "run --cpu x",
            "run --runs 0",
            "run --runs 21",
            "run --runs many",
            "run --label on --label off",
            "run --dir",
            "run --out a.json",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_compare_and_help() {
        assert_eq!(
            parse(&args("compare a.json results")).unwrap(),
            Command::Compare(vec!["a.json".into(), "results".into()])
        );
        assert_eq!(
            parse(&args("compare")).unwrap(),
            Command::Compare(vec![DEFAULT_DIR.into()])
        );
        assert!(parse(&args("compare --all")).is_err());
        assert_eq!(parse(&[]).unwrap(), Command::Help);
        assert_eq!(parse(&args("--help")).unwrap(), Command::Help);
        assert_eq!(parse(&args("--version")).unwrap(), Command::Version);
        assert!(parse(&args("bench")).is_err());
    }
}
