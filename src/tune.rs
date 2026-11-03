//! Batch sizing and the measurement loop, independent of what a batch does.

/// How long batches should take and how many to run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plan {
    /// Timer ticks per second (`QueryPerformanceFrequency`).
    pub(crate) freq: u64,
    pub(crate) batches: usize,
    pub(crate) warmup_batches: usize,
    pub(crate) min_ms: u64,
    pub(crate) target_ms: u64,
    pub(crate) max_ms: u64,
}

impl Plan {
    pub(crate) const fn standard(freq: u64) -> Self {
        Self {
            freq,
            batches: 50,
            warmup_batches: 5,
            min_ms: 20,
            target_ms: 35,
            max_ms: 50,
        }
    }

    fn ticks(&self, ms: u64) -> u64 {
        self.freq.saturating_mul(ms) / 1000
    }
}

/// The result of a measurement: the batch size and each measured batch's duration in ticks.
#[derive(Debug)]
pub(crate) struct Measured {
    pub(crate) ops_per_batch: u64,
    pub(crate) batch_ticks: Vec<u64>,
}

impl Measured {
    /// Nanoseconds per unit for each batch, where each operation counts as `units_per_op`
    /// units (2 for a ping-pong round trip that is two context switches).
    pub(crate) fn ns_per_unit(&self, freq: u64, units_per_op: u32) -> Vec<f64> {
        #[expect(
            clippy::cast_precision_loss,
            reason = "tick and op counts stay far below 2^52"
        )]
        let denom = freq as f64 * self.ops_per_batch as f64 * f64::from(units_per_op);
        self.batch_ticks
            .iter()
            .map(|&t| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "tick counts stay far below 2^52"
                )]
                let t = t as f64;
                t * 1e9 / denom
            })
            .collect()
    }
}

/// Largest batch the tuner will try before giving up.
const MAX_OPS: u64 = 1 << 40;
/// Attempts to land a batch inside the window after the first scaling step.
const FIT_ATTEMPTS: usize = 8;

/// Sizes the batch so one takes `min_ms..=max_ms`, warms up, then times `plan.batches` batches.
///
/// `run(n)` performs `n` operations and returns the elapsed ticks. Any error from `run`
/// aborts the measurement and is returned as is.
pub(crate) fn measure<F>(plan: &Plan, mut run: F) -> Result<Measured, String>
where
    F: FnMut(u64) -> Result<u64, String>,
{
    let target = plan.ticks(plan.target_ms).max(1);
    let window = plan.ticks(plan.min_ms)..=plan.ticks(plan.max_ms);

    // Grow until a batch is long enough to extrapolate from (an eighth of the target).
    let mut n = 1u64;
    let mut t = run(n)?;
    while t < target / 8 {
        n = n.saturating_mul(2);
        if n > MAX_OPS {
            return Err(format!(
                "{MAX_OPS} operations still took under {} ms",
                plan.target_ms / 8
            ));
        }
        t = run(n)?;
    }

    let mut fitted = false;
    for _ in 0..FIT_ATTEMPTS {
        n = scale(n, t, target);
        t = run(n)?;
        if window.contains(&t) {
            fitted = true;
            break;
        }
    }
    if !fitted {
        return Err(format!(
            "could not size a batch to {}-{} ms (last: {n} ops in {:.1} ms)",
            plan.min_ms,
            plan.max_ms,
            ticks_to_ms(t, plan.freq)
        ));
    }

    for _ in 0..plan.warmup_batches {
        run(n)?;
    }
    let batch_ticks = (0..plan.batches)
        .map(|_| run(n))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Measured {
        ops_per_batch: n,
        batch_ticks,
    })
}

/// `n * target / elapsed`, at least 1.
fn scale(n: u64, elapsed: u64, target: u64) -> u64 {
    let scaled = u128::from(n) * u128::from(target) / u128::from(elapsed.max(1));
    u64::try_from(scaled).unwrap_or(u64::MAX).clamp(1, MAX_OPS)
}

pub(crate) fn ticks_to_ms(ticks: u64, freq: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "tick counts stay far below 2^52"
    )]
    let ms = ticks as f64 * 1000.0 / freq as f64;
    ms
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQ: u64 = 10_000_000; // 10 MHz, the usual QPC rate on Windows 10/11.

    #[test]
    fn sizes_batch_into_window() {
        let plan = Plan::standard(FREQ);
        // 150 ns per op = 1.5 ticks per op.
        let m = measure(&plan, |n| Ok(n * 3 / 2)).unwrap();
        assert_eq!(m.batch_ticks.len(), 50);
        for &t in &m.batch_ticks {
            assert!((200_000..=500_000).contains(&t), "{t}");
        }
        let ns = m.ns_per_unit(FREQ, 1);
        assert!((ns[0] - 150.0).abs() < 0.01, "{}", ns[0]);
        // A round trip counted as two switches halves the per-unit time.
        assert!((m.ns_per_unit(FREQ, 2)[0] - 75.0).abs() < 0.01);
    }

    #[test]
    fn slow_operations_get_small_batches() {
        let plan = Plan::standard(FREQ);
        // 40 us per op (thread creation scale).
        let m = measure(&plan, |n| Ok(n * 400)).unwrap();
        assert!(
            (500..=1250).contains(&m.ops_per_batch),
            "{}",
            m.ops_per_batch
        );
    }

    #[test]
    fn counts_warmup_and_measured_batches() {
        let plan = Plan::standard(FREQ);
        let mut sized_calls = 0;
        let mut size = 0;
        measure(&plan, |n| {
            if n == size {
                sized_calls += 1;
            } else {
                size = n;
                sized_calls = 1;
            }
            Ok(n * 2)
        })
        .unwrap();
        // fitting batch + warm-up + measured batches, all at the final size
        assert_eq!(sized_calls, 1 + 5 + 50);
    }

    #[test]
    fn error_aborts_immediately() {
        let plan = Plan::standard(FREQ);
        let mut calls = 0;
        let err = measure(&plan, |n| {
            calls += 1;
            if calls == 30 {
                Err("unexpected status".into())
            } else {
                Ok(n * 2)
            }
        })
        .unwrap_err();
        assert_eq!(err, "unexpected status");
        assert_eq!(calls, 30);
    }

    #[test]
    fn erratic_timing_fails_to_fit() {
        let plan = Plan::standard(FREQ);
        let mut flip = false;
        // Every call is either 100x faster or 100x slower than the previous one, so a
        // size scaled from one batch never fits the next.
        let err = measure(&plan, |n| {
            flip = !flip;
            Ok(if flip { n * 100 } else { n * 10_000 })
        })
        .unwrap_err();
        assert!(err.contains("could not size"), "{err}");
    }

    #[test]
    fn never_slow_enough_fails() {
        let plan = Plan::standard(FREQ);
        assert!(measure(&plan, |_| Ok(0)).is_err());
    }
}
