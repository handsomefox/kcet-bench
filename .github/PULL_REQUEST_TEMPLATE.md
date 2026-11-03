## Summary

<!-- What changed, and why? -->

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Windows cross-build or manual testing, if the change touches Windows-only code

## Safety impact

<!-- Describe any effect on what a batch times, the result checks that abort a test, the label rule, the process setup (priority, pinning, power throttling), or the meaning of a JSON field. Write "none" if there is none. -->
