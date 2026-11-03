# kcet-bench

[![CI](https://github.com/handsomefox/kcet-bench/actions/workflows/ci.yml/badge.svg)](https://github.com/handsomefox/kcet-bench/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

kcet-bench measures the cost of Windows kernel-mode hardware-enforced stack protection, the Windows 11 feature that turns on CET shadow stacks in the kernel. With it on, every kernel `call` also pushes the return address to a shadow stack, and every `ret` checks it. kcet-bench times kernel paths with few and many call/ret pairs, with the feature off and then on, and reports the difference for each path.

## What it measures

Each test sizes a batch to take 20-50 ms, runs 5 warm-up batches, then times 50 batches with `QueryPerformanceCounter`. It reports nanoseconds per unit: the median, p5, p95, and minimum over the 50 batches.

| Test | Operation | Unit |
| --- | --- | --- |
| `syscall_shallow` | `SetEvent` on a manual-reset event that is already signaled | call |
| `syscall_deep` | `GetFileAttributesW` on a 64-byte file in `%TEMP%`, through the I/O manager, the filesystem, and its filter drivers | call |
| `ctx_switch` | Two threads on the same CPU pass control back and forth through two auto-reset events with `SignalObjectAndWait`. One round trip is two switches. | switch |
| `thread_create` | `CreateThread` with a start routine that returns 0, then `WaitForSingleObject` and `CloseHandle` | thread |
| `kernel_seh_baseline` | `NtQueryInformationProcess(ProcessBasicInformation)` into a valid buffer | call |
| `kernel_seh` | The same call into a committed `PAGE_NOACCESS` page. The kernel faults when it copies the result out, and its own exception handler returns `STATUS_ACCESS_VIOLATION`. | call |

The tool checks the result of every call. If a test gets an unexpected result, it stops, and the run file records the reason instead of numbers.

All measuring threads run at `HIGH_PRIORITY_CLASS`, pinned to one logical CPU, with power throttling turned off for the process. The executable is not marked `/CETCOMPAT`, so user-mode shadow stacks stay off and only the kernel's cost changes. `run` confirms that before it measures.

## Requirements

- Windows 11 on a CPU with shadow stacks: Intel 11th generation Core or later, or AMD Zen 3 or later.
- Memory integrity turned on for both settings. Kernel stack protection requires it, and keeping it on means only one thing changes between runs.

## Install

Download the latest release from the [releases page](https://github.com/handsomefox/kcet-bench/releases). Unpack `kcet-bench-<version>-windows-x86_64.zip` and run `kcet-bench.exe` from the folder inside it. `SHA256SUMS` beside the archive holds its checksum. To build it yourself, see [Development](#development).

## Run protocol

The setting changes only at boot, and the first minutes after a boot are noisy. For each boot:

1. Open **Windows Security** > **Device security** > **Core isolation details**. Turn **Kernel-mode Hardware-enforced Stack Protection** on or off, and leave **Memory integrity** on.
2. Restart Windows.
3. After you sign in, leave the PC idle for 3 minutes.
4. Run `wsl --shutdown`, so the WSL virtual machine stops using CPU time.
5. Close every other app.
6. Open an elevated PowerShell in the folder that holds `kcet-bench.exe`, and run the tool 3 times:

    ```powershell
    .\kcet-bench.exe run --label off --out off-boot1-run1.json
    .\kcet-bench.exe run --label off --out off-boot1-run2.json
    .\kcet-bench.exe run --label off --out off-boot1-run3.json
    ```

    Use `--label on` while the feature is on.

Alternate the setting from boot to boot: off, on, off, on. Stop when each setting has at least 3 boots.

`run` refuses to start when memory integrity is not running, when the label does not match the system, or when the output file already exists. It pins to logical CPU 2 by default. To use another CPU, pass `--cpu <n>`, and use the same CPU for every run. CPU 0 is not allowed, because it handles most interrupts. On a CPU with performance and efficiency cores, pick a performance core.

## Compare the runs

```powershell
.\kcet-bench.exe compare (Get-ChildItem *.json).FullName
```

For each test, `compare` prints the median of the run medians for each label, the change from `off` to `on` in percent, and the spread of each label: the lowest and highest run median. A change is "within noise" when the difference between the two medians is smaller than the larger of the two spreads. The verdict needs at least 2 runs per label. The notes under the table name aborted runs, labels with fewer than 3 boots, and runs that mix Windows builds, CPUs, power plans, or pinned CPUs.

`compare` also runs on Linux, so you can read the files from WSL with `cargo run --release -- compare /mnt/c/Users/<you>/kcet-bench/*.json`.

Each run file holds the label, the time, the Windows build, the CPU, the power plan, the `Win32_DeviceGuard` state, every batch's time per unit, and the summary for each test.

## Development

Everything except `run` builds and tests on any OS, including Linux. `run` measures the Windows kernel, so it only works on Windows.

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs those three commands on Ubuntu and the last two on Windows, where the tests also call each measured kernel path. It also runs `cargo audit` and `cargo machete`. Linux Clippy skips the Windows-only code, so before you push a change to `src/win/`, also lint for the Windows target:

```sh
cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

To cross-build the Windows x86-64 executable from Linux or WSL2, use `cargo-xwin`:

```sh
cargo xwin build --release --target x86_64-pc-windows-msvc
```

The executable is `target/x86_64-pc-windows-msvc/release/kcet-bench.exe`. From WSL2, copy it to a Windows folder, such as `/mnt/c/Users/<you>/kcet-bench/`, and run it from there.

To pack the same archive and `SHA256SUMS` a release publishes, under `dist/`:

```sh
bash scripts/package-windows.sh
```

The packaging script needs `cargo-xwin` 0.23.1, `jq`, `zip`, `unzip`, and GNU `sha256sum`. It checks the archive contents and the checksums before it reports success.

## License

Licensed under the [MIT License](LICENSE).
