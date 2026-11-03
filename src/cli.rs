//! Command-line parsing. No framework: there are two commands and three flags.

use std::path::PathBuf;

use crate::report::Label;

/// The logical CPU measuring threads are pinned to unless `--cpu` says otherwise.
pub(crate) const DEFAULT_CPU: u32 = 2;

pub(crate) const USAGE: &str = "\
kcet-bench: measures what Windows kernel-mode hardware-enforced stack protection costs

Usage:
  kcet-bench run --label <on|off> --out <file.json> [--cpu <n>]
  kcet-bench compare <file.json>...
  kcet-bench --help | --version

run      Measure every test once and write the results to a new JSON file.
         --label  The kernel stack protection setting this boot runs with. The run
                  refuses to start when the label does not match the system.
         --out    The file to create. An existing file is never overwritten.
         --cpu    Logical CPU to pin every measuring thread to (default 2, never 0).
compare  Group run files by label and report each test's delta and noise.
";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    pub(crate) label: Label,
    pub(crate) out: PathBuf,
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
                return Err(format!("compare takes only file names, got {flag}"));
            }
            if rest.is_empty() {
                return Err("compare needs at least one result file".into());
            }
            Ok(Command::Compare(rest.iter().map(PathBuf::from).collect()))
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut label = None;
    let mut out = None;
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
            "--out" => set_once(&mut out, PathBuf::from(value()?), flag)?,
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
        label: label.ok_or("run needs --label on|off")?,
        out: out.ok_or("run needs --out <file.json>")?,
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
    fn parses_run() {
        assert_eq!(
            parse(&args("run --label on --out a.json")).unwrap(),
            Command::Run(RunArgs {
                label: Label::On,
                out: "a.json".into(),
                cpu: DEFAULT_CPU
            })
        );
        assert_eq!(
            parse(&args("run --cpu 6 --out b.json --label off")).unwrap(),
            Command::Run(RunArgs {
                label: Label::Off,
                out: "b.json".into(),
                cpu: 6
            })
        );
    }

    #[test]
    fn rejects_bad_run_flags() {
        for bad in [
            "run --out a.json",
            "run --label on",
            "run --label maybe --out a.json",
            "run --label on --out a.json --cpu 0",
            "run --label on --out a.json --cpu x",
            "run --label on --label off --out a.json",
            "run --label on --out",
            "run --label on --out a.json --fast",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_compare_and_help() {
        assert_eq!(
            parse(&args("compare a.json b.json")).unwrap(),
            Command::Compare(vec!["a.json".into(), "b.json".into()])
        );
        assert!(parse(&args("compare")).is_err());
        assert!(parse(&args("compare --all")).is_err());
        assert_eq!(parse(&[]).unwrap(), Command::Help);
        assert_eq!(parse(&args("--help")).unwrap(), Command::Help);
        assert_eq!(parse(&args("--version")).unwrap(), Command::Version);
        assert!(parse(&args("bench")).is_err());
    }
}
