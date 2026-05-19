//! Nullability / default-marker byte from `SYSCOLUMN` (Phase 6, WP-6D).
//!
//! Each `SYSCOLUMN` row exposes a single byte (`nulls_flag` on
//! [`crate::SysColumn`]) that conflates SA17's nullability, default
//! presence, and "computed/identity" markers. Without access to the
//! authoritative SA17 catalog spec, we ship a histogram-level decoder
//! that tallies observed values and provides one representative column
//! per value, so downstream callers can audit the data themselves.
//!
//! ## Observed pattern on Rock Castle (informational only)
//!
//! ```text
//!   byte    typical context
//!   0x01    simple boolean (is_*)
//!   0x02    required scalar / foreign key (account_id, amount_amt)
//!   0x03    short text scalar (po_num)
//!   0x06    optional foreign key (doc_num_h)
//!   0x09    nullable text (memo, address fields)
//!   0x0d    timestamps and enum-like (delivery_date)
//!   0x13    extended identity (user_dn)
//!   0x15    audit columns (creator)
//!   0x17    procedure metadata (proc_name)
//!   0x18    boolean fields with default (is_build, is_receipt)
//! ```
//!
//! These groupings are heuristic and should NOT be treated as
//! authoritative until the bit layout is reverse-engineered.

use std::collections::BTreeMap;

use opensqlany::{ApModel, PageStore};

use crate::iter_syscolumns;

/// One bucket in the [`histogram`] output.
#[derive(Debug, Clone)]
pub struct NullsFlagBucket {
    /// The raw `nulls_flag` byte.
    pub flag: u8,
    /// How many SYSCOLUMN rows share this byte value.
    pub count: usize,
    /// Up to four column names that share this byte, for context.
    pub sample_columns: Vec<String>,
}

/// Build a histogram of the `nulls_flag` byte across every
/// `SYSCOLUMN` row in `store`, sorted ascending by byte value.
pub fn histogram(store: &PageStore, model: &ApModel) -> Vec<NullsFlagBucket> {
    let mut counts: BTreeMap<u8, (usize, Vec<String>)> = BTreeMap::new();
    for c in iter_syscolumns(store, model) {
        let entry = counts.entry(c.nulls_flag).or_insert((0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 4 && !entry.1.contains(&c.name) {
            entry.1.push(c.name);
        }
    }
    counts
        .into_iter()
        .map(|(flag, (count, sample_columns))| NullsFlagBucket {
            flag,
            count,
            sample_columns,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NullsFlagBucket` is purely a value type; the only real test we
    /// can run here is that the field destructuring is stable.
    #[test]
    fn bucket_fields_round_trip() {
        let b = NullsFlagBucket {
            flag: 0x18,
            count: 42,
            sample_columns: vec!["is_build".into(), "is_receipt".into()],
        };
        assert_eq!(b.flag, 0x18);
        assert_eq!(b.count, 42);
        assert_eq!(b.sample_columns.len(), 2);
    }
}
