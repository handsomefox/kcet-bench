//! kcet-bench: measures what Windows kernel-mode hardware-enforced stack protection costs.

mod cli;
mod compare;
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the Windows-only run command uses these")
)]
mod guard;
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the Windows-only run command uses these")
)]
mod report;
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the Windows-only run command uses these")
)]
mod stats;
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the Windows-only run command uses these")
)]
mod tune;
#[cfg(windows)]
mod win;

use std::io::{self, Write as _};
use std::process::ExitCode;

use cli::Command;

fn main() -> ExitCode {
    let args = match std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(bad) => {
            eprint_line(&format!(
                "error: argument is not valid Unicode: {}",
                bad.display()
            ));
            return ExitCode::from(2);
        }
    };
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprint_line(&format!("error: {e}\n\n{}", cli::USAGE));
            return ExitCode::from(2);
        }
    };
    let result = match command {
        Command::Help => {
            print_text(cli::USAGE);
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            print_text(&format!("kcet-bench {}\n", env!("CARGO_PKG_VERSION")));
            Ok(ExitCode::SUCCESS)
        }
        Command::Compare(files) => compare_files(&files).map(|text| {
            print_text(&text);
            ExitCode::SUCCESS
        }),
        Command::Run(args) => run(&args),
    };
    result.unwrap_or_else(|e| {
        eprint_line(&format!("error: {e}"));
        ExitCode::FAILURE
    })
}

fn compare_files(files: &[std::path::PathBuf]) -> Result<String, String> {
    let mut runs = Vec::with_capacity(files.len());
    for path in files {
        let display = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{display}: {e}"))?;
        let report = serde_json::from_str(&text).map_err(|e| format!("{display}: {e}"))?;
        let loaded = compare::Loaded {
            path: display,
            report,
        };
        compare::check_schema(&loaded)?;
        runs.push(loaded);
    }
    Ok(compare::render(&runs))
}

#[cfg(windows)]
fn run(args: &cli::RunArgs) -> Result<ExitCode, String> {
    win::run(args)
}

#[cfg(not(windows))]
fn run(_: &cli::RunArgs) -> Result<ExitCode, String> {
    Err("run measures the Windows kernel, so it only works on Windows. Build with `make build` and run the .exe there".into())
}

/// Writes to stdout. A closed pipe is not worth a panic, so write errors are dropped.
fn print_text(text: &str) {
    let _ = io::stdout().lock().write_all(text.as_bytes());
}

/// Writes one line to stderr, dropping write errors like `print_text`.
pub(crate) fn eprint_line(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}
