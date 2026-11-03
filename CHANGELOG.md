# Changelog

## 0.1.0

- First release. `kcet-bench run` measures six kernel paths on one pinned CPU and writes one JSON file per run: a short syscall, a deep filesystem syscall, a context switch, thread creation, and a kernel fault that the kernel's own exception handler catches, with a no-fault baseline.
- `run` refuses to start unless memory integrity (HVCI) is running and `--label` matches whether kernel-mode hardware-enforced stack protection is running.
- `kcet-bench compare` groups run files by label and reports each test's change from `off` to `on`, with the run-to-run spread of each label and whether the change is within that spread.
