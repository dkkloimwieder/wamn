//! Exact numeric conversions for the measurement benches.
//!
//! A bench reports rates and percentiles, so it moves counts into `f64` and
//! durations back out again. Every one of those steps is written here, once,
//! and each says what it does to a value that will not fit. `as` hides that
//! choice; these do not.

/// Two to the fifty-third: the last integer an `f64` holds exactly.
const EXACT_LIMIT: u64 = 1 << 53;

/// One measured count as `f64`.
///
/// Bench counts stay far below [`EXACT_LIMIT`], where the conversion is exact.
/// A larger count saturates there rather than reporting a silently rounded
/// rate.
#[must_use]
pub fn count_f64(count: u64) -> f64 {
    let bounded = count.min(EXACT_LIMIT);
    let high = u32::try_from(bounded >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(bounded & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// One measured count as `f64`, from a signed counter. A negative count is a
/// bug in the caller's arithmetic, so it reports zero rather than a rate with
/// the wrong sign.
#[must_use]
pub fn signed_count_f64(count: i64) -> f64 {
    count_f64(u64::try_from(count).unwrap_or(0))
}

/// One measured length as `f64`.
#[must_use]
pub fn len_f64(len: usize) -> f64 {
    count_f64(u64::try_from(len).unwrap_or(u64::MAX))
}

/// A finite, non-negative `f64` as a whole count.
///
/// `Duration` carries the conversion, so no float-to-integer cast is needed.
/// A negative or non-finite value reports zero.
#[must_use]
pub fn whole_count(value: f64) -> u64 {
    std::time::Duration::try_from_secs_f64(value.round().max(0.0))
        .map_or(0, |count| count.as_secs())
}

/// A count as an index. A count past the address width saturates rather than
/// wrapping into a different element.
#[must_use]
pub fn index(count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// A length as a signed count, for a database column that stores one.
#[must_use]
pub fn signed_len(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// A length as a `u32`, for a protocol field that carries one.
#[must_use]
pub fn narrow_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{count_f64, index, len_f64, narrow_len, signed_count_f64, signed_len, whole_count};

    #[test]
    fn a_count_crosses_to_f64_exactly_and_saturates_past_the_exact_limit() {
        for (count, expected) in [
            (0_u64, 0.0_f64),
            (1, 1.0),
            (1_000_000, 1_000_000.0),
            ((1 << 53) - 1, 9_007_199_254_740_991.0),
        ] {
            assert!((count_f64(count) - expected).abs() < f64::EPSILON);
        }
        assert!((count_f64(u64::MAX) - count_f64(1 << 53)).abs() < f64::EPSILON);
    }

    #[test]
    fn a_negative_count_reports_zero_rather_than_a_signed_rate() {
        assert!(signed_count_f64(-1).abs() < f64::EPSILON);
        assert!((signed_count_f64(42) - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_float_reports_a_whole_count_and_never_a_negative_one() {
        assert_eq!(whole_count(4.4), 4);
        assert_eq!(whole_count(4.6), 5);
        assert_eq!(whole_count(-1.0), 0);
        assert_eq!(whole_count(f64::NAN), 0);
    }

    #[test]
    fn widths_saturate_rather_than_wrap() {
        assert_eq!(index(7), 7);
        assert_eq!(signed_len(7), 7);
        assert_eq!(narrow_len(7), 7);
        assert!((len_f64(7) - 7.0).abs() < f64::EPSILON);
    }
}
