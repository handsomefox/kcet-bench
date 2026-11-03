//! Order statistics over per-batch timings.

use serde::{Deserialize, Serialize};

/// Summary of a sample of per-operation times, in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Summary {
    pub(crate) median: f64,
    pub(crate) p5: f64,
    pub(crate) p95: f64,
    pub(crate) min: f64,
}

/// Summarizes `values`. Returns `None` for an empty sample or one containing NaN.
///
/// The median of an even-sized sample is the mean of the two middle values. `p5` and `p95`
/// use the nearest-rank definition: the smallest value with at least p% of the sample at or
/// below it. For 50 batches that is the 3rd and the 48th smallest value.
pub(crate) fn summarize(values: &[f64]) -> Option<Summary> {
    if values.is_empty() || values.iter().any(|v| v.is_nan()) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Summary {
        median: median_of_sorted(&sorted),
        p5: nearest_rank(&sorted, 5),
        p95: nearest_rank(&sorted, 95),
        min: sorted[0],
    })
}

/// Median of `values`, or `None` when empty or when a value is NaN.
pub(crate) fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| v.is_nan()) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(median_of_sorted(&sorted))
}

fn median_of_sorted(sorted: &[f64]) -> f64 {
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    }
}

/// Nearest-rank percentile of a non-empty sorted sample, for `percent` in 1..=100.
fn nearest_rank(sorted: &[f64], percent: usize) -> f64 {
    // rank = ceil(percent / 100 * n), 1-based.
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifty_values_use_nearest_rank() {
        // 1.0 ..= 50.0, shuffled so the sort is exercised.
        let mut values: Vec<f64> = (1..=50).map(f64::from).collect();
        values.reverse();
        values.swap(3, 40);
        let s = summarize(&values).unwrap();
        assert!((s.median - 25.5).abs() < f64::EPSILON);
        assert!((s.p5 - 3.0).abs() < f64::EPSILON);
        assert!((s.p95 - 48.0).abs() < f64::EPSILON);
        assert!((s.min - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn odd_sample_median_is_middle_value() {
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
    }

    #[test]
    fn single_value() {
        let s = summarize(&[7.0]).unwrap();
        assert_eq!((s.median, s.p5, s.p95, s.min), (7.0, 7.0, 7.0, 7.0));
    }

    #[test]
    fn rejects_empty_and_nan() {
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[1.0, f64::NAN]), None);
        assert_eq!(median(&[]), None);
    }
}
