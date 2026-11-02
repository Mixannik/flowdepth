//! POC and value-area selection over an adapter's existing ordered price rows.
//! Row adjacency, including zero-volume rows, is deliberately preserved.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueArea {
    pub poc: usize,
    pub low: usize,
    pub high: usize,
    pub total: f64,
    pub included: f64,
}

/// Select a contiguous value area, starting at the POC. Equal maxima select the
/// last (higher-price) row; equal neighbouring weights expand upwards first.
/// A zero share still selects the POC. Invalid inputs or no volume return None.
pub fn value_area(weights: &[f64], share: f64) -> Option<ValueArea> {
    if weights.is_empty()
        || !share.is_finite()
        || !(0.0..=1.0).contains(&share)
        || weights.iter().any(|w| !w.is_finite() || *w < 0.0)
    {
        return None;
    }
    let total: f64 = weights.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let poc = weights
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))?
        .0;
    let (mut low, mut high, mut included) = (poc, poc, weights[poc]);
    let target = total * share;
    while included < target && (low > 0 || high + 1 < weights.len()) {
        let below = if low > 0 { weights[low - 1] } else { -1.0 };
        let above = if high + 1 < weights.len() {
            weights[high + 1]
        } else {
            -1.0
        };
        let weight = if above >= below {
            high += 1;
            weights[high]
        } else {
            low -= 1;
            weights[low]
        };
        included += weight;
        if !included.is_finite() {
            return None;
        }
    }
    Some(ValueArea {
        poc,
        low,
        high,
        total,
        included,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poc_and_neighbour_ties_choose_upper_rows() {
        let tied_poc = value_area(&[4.0, 4.0], 0.0).unwrap();
        assert_eq!((tied_poc.poc, tied_poc.low, tied_poc.high), (1, 1, 1));
        let tied_neighbours = value_area(&[2.0, 5.0, 2.0], 0.7).unwrap();
        assert_eq!((tied_neighbours.low, tied_neighbours.high), (1, 2));
        assert_eq!(tied_neighbours.included, 7.0);
    }

    #[test]
    fn dense_zero_rows_and_sparse_rows_keep_distinct_adjacency() {
        let dense = value_area(&[4.0, 0.0, 5.0, 0.0, 3.0], 0.7).unwrap();
        assert_eq!((dense.poc, dense.low, dense.high), (2, 0, 4));
        assert_eq!((dense.total, dense.included), (12.0, 12.0));
        let sparse = value_area(&[4.0, 5.0, 3.0], 0.7).unwrap();
        assert_eq!((sparse.poc, sparse.low, sparse.high), (1, 0, 1));
        assert_eq!((sparse.total, sparse.included), (12.0, 9.0));
    }

    #[test]
    fn zero_share_full_share_and_single_row_have_defined_results() {
        let zero = value_area(&[1.0, 4.0, 2.0], 0.0).unwrap();
        assert_eq!((zero.low, zero.high, zero.included), (1, 1, 4.0));
        let full = value_area(&[1.0, 4.0, 2.0], 1.0).unwrap();
        assert_eq!(
            (full.low, full.high, full.total, full.included),
            (0, 2, 7.0, 7.0)
        );
        let single = value_area(&[3.0], 1.0).unwrap();
        assert_eq!((single.poc, single.low, single.high), (0, 0, 0));
        assert_eq!(single.total, single.included);
    }

    #[test]
    fn invalid_mass_share_or_total_never_produces_levels() {
        for weights in [
            &[][..],
            &[0.0, 0.0],
            &[-1.0, 2.0],
            &[f64::NAN, 2.0],
            &[f64::INFINITY],
            &[f64::MAX, f64::MAX],
        ] {
            assert!(value_area(weights, 0.7).is_none());
        }
        for share in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(value_area(&[1.0], share).is_none());
        }
    }
}
