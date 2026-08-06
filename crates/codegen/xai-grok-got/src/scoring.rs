//! The paper's local scoring functions, as literal implementations of §5.
//!
//! Both are "error scope" metrics: zero is perfect and larger is worse, so a
//! [`crate::OpKind::KeepBestN`] over them wants `higher_is_better: false`.
//! Both are pure, so a decomposition that uses them spends no tokens on
//! scoring and has no scoring variance — which is exactly why the paper's
//! sorting and set-intersection results are tighter than its document-merging
//! results, where the model has to judge itself.

use std::collections::HashMap;

/// Sorting error scope (§5.1): `X + Y`.
///
/// `X` counts adjacent out-of-order pairs; `Y` counts how far the output's
/// value histogram drifts from the input's. The second term is the one that
/// matters in practice — the paper's models mostly produce plausibly sorted
/// output with the wrong *multiset*, dropping or duplicating elements, which a
/// pure orderedness check would score as perfect.
pub fn sorting_error_scope(input: &[u32], output: &[u32]) -> u64 {
    let disorder = output.windows(2).filter(|pair| pair[0] > pair[1]).count() as u64;

    let mut frequency: HashMap<u32, i64> = HashMap::new();
    for &value in output {
        *frequency.entry(value).or_default() += 1;
    }
    for &value in input {
        *frequency.entry(value).or_default() -= 1;
    }
    let drift: u64 = frequency.values().map(|delta| delta.unsigned_abs()).sum();

    disorder + drift
}

/// Set-intersection error scope (§5.2): `X1 + X2 + Xd`.
///
/// Extra elements, missing elements, and duplicates. Duplicates count because
/// the model returns a *list* in natural language, so it can restate a member
/// and still look correct to a set-membership check.
pub fn set_intersection_error_scope(a: &[i64], b: &[i64], output: &[i64]) -> u64 {
    let left: std::collections::HashSet<i64> = a.iter().copied().collect();
    let right: std::collections::HashSet<i64> = b.iter().copied().collect();
    let expected: std::collections::HashSet<i64> = left.intersection(&right).copied().collect();
    let produced: std::collections::HashSet<i64> = output.iter().copied().collect();

    let spurious = produced.difference(&expected).count() as u64;
    let missing = expected.difference(&produced).count() as u64;
    let duplicates = (output.len() - produced.len()) as u64;

    spurious + missing + duplicates
}

/// Flip an error scope into the paper's "positive score" (Appendix A):
/// `max(n - error, 0)`, higher is better.
pub fn positive_score(problem_size: usize, error_scope: u64) -> u64 {
    (problem_size as u64).saturating_sub(error_scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correct_sort_scores_zero() {
        let input = [3, 1, 2, 1];
        assert_eq!(sorting_error_scope(&input, &[1, 1, 2, 3]), 0);
    }

    /// The failure the metric exists to catch: perfectly ordered output that
    /// lost an element. Orderedness alone would call this flawless.
    #[test]
    fn ordered_output_with_a_dropped_element_is_still_penalized() {
        let input = [3, 1, 2, 1];
        assert_eq!(
            sorting_error_scope(&input, &[1, 2, 3]),
            1,
            "one missing 1 is one unit of histogram drift",
        );
        assert_eq!(
            sorting_error_scope(&input, &[1, 1, 1, 2, 3]),
            1,
            "an extra 1 is penalized the same",
        );
    }

    #[test]
    fn disorder_and_drift_add() {
        let input = [1, 2, 3];
        // Reversed: two out-of-order pairs, histogram intact.
        assert_eq!(sorting_error_scope(&input, &[3, 2, 1]), 2);
        // Reversed AND missing 2: one out-of-order pair plus one drift.
        assert_eq!(sorting_error_scope(&input, &[3, 1]), 2);
    }

    #[test]
    fn intersection_counts_spurious_missing_and_duplicate_separately() {
        let a = [1, 2, 3, 4];
        let b = [3, 4, 5, 6];
        assert_eq!(set_intersection_error_scope(&a, &b, &[3, 4]), 0);
        assert_eq!(set_intersection_error_scope(&a, &b, &[3]), 1, "missing 4",);
        assert_eq!(
            set_intersection_error_scope(&a, &b, &[3, 4, 5]),
            1,
            "5 is in b but not a",
        );
        assert_eq!(
            set_intersection_error_scope(&a, &b, &[3, 4, 4]),
            1,
            "a restated member is a duplicate, not a second hit",
        );
    }

    #[test]
    fn positive_score_is_the_clamped_complement() {
        assert_eq!(positive_score(32, 0), 32);
        assert_eq!(positive_score(32, 5), 27);
        assert_eq!(positive_score(32, 40), 0, "clamped, never negative");
    }
}
