# Repository guidelines

The [README](README.md) covers what kcet-bench measures, how to build it, and the run protocol. This page covers the rules for changing it.

## Where code goes

One binary, split so that everything portable is testable on Linux:

- `src/cli.rs`, `src/compare.rs`, `src/guard.rs`, `src/report.rs`, `src/session.rs`, `src/stats.rs`, and `src/tune.rs` are portable. They hold argument parsing, the comparison and its noise rule, the Device Guard parser and label rule, the JSON model, the results folder (file names, boot numbers, and the next protocol step), the order statistics, and the batch-sizing loop.
- `src/win/` is Windows-only, behind `cfg(windows)`. `bench.rs` holds the measured operations, `sysinfo.rs` the system description, `sys.rs` the shared Win32 wrappers, and `mod.rs` the `run` command.

Put logic that does not call Windows in a portable module, so that it gets a unit test on Linux.

## Do not weaken the measurement

A number in a run file is a claim about the kernel. Keep every rule that makes it true:

- Do not mark the executable `/CETCOMPAT`, and keep the check that refuses to run when user-mode shadow stacks are on. Only the kernel's cost may differ between labels.
- `Bench::batch` holds the operation, its result check, and the loop counter, and nothing else. No allocation, formatting, `std::thread`, or extra system calls inside the loop, except on the error path.
- Check every result inside the loop. An unexpected result returns an error, which aborts the test and keeps its numbers out of the file. `kernel_seh` must see `STATUS_ACCESS_VIOLATION` on every call.
- `run` raises the process to `HIGH_PRIORITY_CLASS`, opts out of power throttling, and pins the process affinity mask to one CPU, so threads created later stay on that CPU. CPU 0 is never allowed.
- `run` queries Device Guard before it raises priority or pins, so PowerShell does not inherit either. HVCI must run under both labels. The label is `on` exactly when kernel stack protection (5) runs, and comes from Windows, never from the user alone. Audit mode (6) fits neither label.
- `run` waits until the system has been up for 3 minutes before it measures.
- Batches take 20-50 ms, timed with `QueryPerformanceCounter` around the whole batch, never around one operation.
- `run` never overwrites an output file.

If a change alters what a test measures or what a JSON field means, bump `SCHEMA_VERSION` in `src/report.rs`. `compare` refuses files with another schema, so old and new runs cannot be mixed.

## Tests

Unit tests live beside their implementations in `#[cfg(test)]` modules. Cover the rejection path for anything that parses input: arguments, the PowerShell output, and run files.

The tests in `src/win/bench.rs` call the real kernel paths, and CI's Windows job runs them. They share one process, so they must never change priority or affinity.

CI cannot reach the parts that need a configured machine. Exercise these by hand on Windows: a full `run` under each label, a second `run` in the same boot (its run numbers continue), the refusal when `--label` does not match, and `compare` over the results folder.

## Bump CI tool pins by hand

`scripts/install-ci-tool.sh` downloads cargo-audit, cargo-machete, actionlint, and zizmor from their release pages and checks each archive against a pinned SHA-256 before it extracts anything. Dependabot cannot bump these pins. To bump one, change its row in the script and take the new hash from the digest GitHub records for the asset:

```sh
gh release view <tag> -R <owner>/<repo> --json assets --jq '.assets[] | select(.name == "<asset>") | .digest'
```

CI runs actionlint, shellcheck, and `zizmor --persona pedantic` on every push. Run all three before you push a workflow change.
