# Changelog

## 0.2.0

- `kcet-bench run` needs no arguments. It reads the kernel stack protection setting from Windows instead of taking `--label`, runs every test 3 times, and names the files itself: `results\<label>-boot<n>-run<n>.json`. Boot and run numbers continue from the files already in the folder. `--out` is gone. Use `--dir` for another folder and `--runs` for another run count. `--label` still refuses a run when the system does not match it.
- After its runs, `run` prints the comparison of everything in the folder and says which way to turn the setting before the next restart.
- `run` waits until Windows has been up for 3 minutes, and warns when WSL or another virtual machine is running or when two boots in a row have the same setting.
- `kcet-bench compare` reads the `results` folder when given no files, and reads every run file in any folder it is given.

## 0.1.0

- First release. `kcet-bench run` measures six kernel paths on one pinned CPU and writes one JSON file per run: a short syscall, a deep filesystem syscall, a context switch, thread creation, and a kernel fault that the kernel's own exception handler catches, with a no-fault baseline.
- `run` refuses to start unless memory integrity (HVCI) is running and `--label` matches whether kernel-mode hardware-enforced stack protection is running.
- `kcet-bench compare` groups run files by label and reports each test's change from `off` to `on`, with the run-to-run spread of each label and whether the change is within that spread.
