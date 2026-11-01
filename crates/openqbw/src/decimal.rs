//! An exact decimal number, the type of a decoded line-item value.

use std::fmt;

/// An exact decimal: `mantissa / 10^scale`, with no rounding.
///
/// Values are kept in lowest terms (no trailing zero in the fraction), so two
/// equal numbers compare equal and print the same. Arithmetic is checked and
/// returns `None` instead of wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decimal {
    mantissa: i128,
    scale: u32,
}

impl Decimal {
    /// Zero.
    pub const ZERO: Decimal = Decimal {
        mantissa: 0,
        scale: 0,
    };

    /// The number `mantissa / 10^scale`, reduced to lowest terms.
    pub fn new(mantissa: i128, scale: u32) -> Self {
        let (mut mantissa, mut scale) = (mantissa, scale);
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        Decimal { mantissa, scale }
    }

    /// The mantissa in lowest terms.
    pub fn mantissa(&self) -> i128 {
        self.mantissa
    }

    /// Digits after the decimal point, with no trailing zero.
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// `true` when the number is zero.
    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    /// `true` when the number is below zero.
    pub fn is_negative(&self) -> bool {
        self.mantissa < 0
    }

    /// The sum, or `None` when it does not fit.
    pub fn checked_add(self, other: Decimal) -> Option<Decimal> {
        let (lo, hi) = if self.scale <= other.scale {
            (self, other)
        } else {
            (other, self)
        };
        let lifted = lo
            .mantissa
            .checked_mul(10i128.checked_pow(hi.scale - lo.scale)?)?;
        Some(Decimal::new(lifted.checked_add(hi.mantissa)?, hi.scale))
    }

    /// The sum of `values`, or `None` when it does not fit.
    pub fn checked_sum<I: IntoIterator<Item = Decimal>>(values: I) -> Option<Decimal> {
        values
            .into_iter()
            .try_fold(Decimal::ZERO, Decimal::checked_add)
    }

    /// The negation, or `None` when it does not fit.
    pub fn checked_neg(self) -> Option<Decimal> {
        self.mantissa.checked_neg().map(|m| Decimal {
            mantissa: m,
            scale: self.scale,
        })
    }
}

impl Default for Decimal {
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.mantissa.unsigned_abs().to_string();
        let sign = if self.mantissa < 0 { "-" } else { "" };
        let scale = self.scale as usize;
        if scale == 0 {
            write!(f, "{sign}{digits}")
        } else if digits.len() > scale {
            let (int, frac) = digits.split_at(digits.len() - scale);
            write!(f, "{sign}{int}.{frac}")
        } else {
            write!(f, "{sign}0.{digits:0>scale$}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_exactly() {
        assert_eq!(Decimal::new(7547, 2).to_string(), "75.47");
        assert_eq!(Decimal::new(-3445, 2).to_string(), "-34.45");
        assert_eq!(Decimal::new(5, 2).to_string(), "0.05");
        assert_eq!(Decimal::new(-5, 3).to_string(), "-0.005");
        assert_eq!(Decimal::new(300, 0).to_string(), "300");
        assert_eq!(Decimal::new(0, 4).to_string(), "0");
    }

    #[test]
    fn is_kept_in_lowest_terms() {
        assert_eq!(Decimal::new(34950, 2), Decimal::new(3495, 1));
        assert_eq!(Decimal::new(34950, 2).to_string(), "349.5");
        assert_eq!(Decimal::new(1200, 2).to_string(), "12");
        assert_eq!(Decimal::new(0, 6), Decimal::ZERO);
    }

    #[test]
    fn adds_across_scales() {
        let a = Decimal::new(7547, 2);
        let b = Decimal::new(-75, 0);
        assert_eq!(a.checked_add(b).unwrap().to_string(), "0.47");
        let neg = Decimal::new(-7547, 2);
        assert!(a.checked_add(neg).unwrap().is_zero());
        let big = Decimal::new(i128::MAX, 0);
        assert_eq!(big.checked_add(Decimal::new(1, 0)), None);
    }
}
