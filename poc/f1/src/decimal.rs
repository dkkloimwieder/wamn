//! Exact-decimal arithmetic for spec evaluation — the POC's hard rule: material
//! quantities and specs are NEVER floats (the catalog types them
//! `numeric(p,s)`; the REST gateway and the seed compiler enforce the same rule
//! on their paths). A decimal travels as its canonical string and is compared /
//! subtracted here as a scaled `i128`, so `12.50 == 12.5` and
//! `|100.000 - 99.950| == 0.050` hold exactly.

use std::cmp::Ordering;

/// Total significant digits a parsed decimal may carry. The F1 catalog tops out
/// at `numeric(12,3)`; the cap exists so scale alignment (`units * 10^diff`)
/// can never overflow `i128` (27 digits + 9 scale shift < 10^38).
const MAX_DIGITS: usize = 27;
/// Maximum fractional digits accepted.
const MAX_SCALE: usize = 9;

/// A parsed exact decimal: `value = units * 10^-scale` (`units` carries the
/// sign). Constructed only through [`Decimal::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    units: i128,
    scale: u32,
}

impl Decimal {
    /// Parse a canonical decimal string: optional `-`, at least one digit,
    /// optionally `.` followed by at least one digit. No exponent, no leading
    /// `.`/trailing `.`, no whitespace, no `+`. `"12"`, `"12.50"`, `"-0.5"` are
    /// accepted; `""`, `"."`, `".5"`, `"12."`, `"1e5"`, `"NaN"` are not.
    pub fn parse(s: &str) -> Result<Decimal, String> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (int_part, frac_part) = match digits.split_once('.') {
            Some((i, f)) => (i, f),
            None => (digits, ""),
        };
        if int_part.is_empty() || (digits.contains('.') && frac_part.is_empty()) {
            return Err(format!("not a decimal: {s:?}"));
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(format!("not a decimal: {s:?}"));
        }
        if int_part.len() + frac_part.len() > MAX_DIGITS {
            return Err(format!("too many digits: {s:?}"));
        }
        if frac_part.len() > MAX_SCALE {
            return Err(format!("too many fractional digits: {s:?}"));
        }
        let mut units: i128 = 0;
        for b in int_part.bytes().chain(frac_part.bytes()) {
            units = units * 10 + i128::from(b - b'0');
        }
        if neg {
            units = -units;
        }
        Ok(Decimal {
            units,
            scale: frac_part.len() as u32,
        })
    }

    /// Rescale both to a common scale. Cannot overflow given the parse caps.
    fn aligned(self, other: Decimal) -> (i128, i128, u32) {
        let scale = self.scale.max(other.scale);
        let a = self.units * 10_i128.pow(scale - self.scale);
        let b = other.units * 10_i128.pow(scale - other.scale);
        (a, b, scale)
    }

    /// Numeric comparison (scale-independent): `12.50 == 12.5`.
    pub fn cmp_value(&self, other: &Decimal) -> Ordering {
        let (a, b, _) = self.aligned(*other);
        a.cmp(&b)
    }

    /// `|self - other|`, at the wider of the two scales.
    pub fn abs_diff(&self, other: &Decimal) -> Decimal {
        let (a, b, scale) = self.aligned(*other);
        Decimal {
            units: (a - b).abs(),
            scale,
        }
    }

    /// Strictly negative (`-0` parses to zero, which is not negative).
    pub fn is_negative(&self) -> bool {
        self.units < 0
    }

    /// Zero at any scale.
    pub fn is_zero(&self) -> bool {
        self.units == 0
    }

    /// Digits left of the point, leading zeros ignored (`0.50` has 0). Used for
    /// `numeric(precision, scale)` range checks, mirroring the seed compiler.
    fn int_digits(&self) -> u32 {
        let mut magnitude = self.units.abs() / 10_i128.pow(self.scale);
        let mut n = 0;
        while magnitude > 0 {
            n += 1;
            magnitude /= 10;
        }
        n
    }

    /// Whether the value fits a Postgres `numeric(precision, scale)` column:
    /// fractional digits within `scale`, integer digits within
    /// `precision - scale`.
    pub fn fits(&self, precision: u32, scale: u32) -> bool {
        self.scale <= scale && self.int_digits() <= precision - scale
    }
}

impl std::fmt::Display for Decimal {
    /// Canonical string at the parsed scale: `Decimal::parse("0.050")` prints
    /// `0.050` (evaluation reasons quote deviations at full stored scale).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.units < 0 { "-" } else { "" };
        let abs = self.units.unsigned_abs();
        if self.scale == 0 {
            return write!(f, "{sign}{abs}");
        }
        let pow = 10_u128.pow(self.scale);
        write!(
            f,
            "{sign}{}.{:0width$}",
            abs / pow,
            abs % pow,
            width = self.scale as usize
        )
    }
}
