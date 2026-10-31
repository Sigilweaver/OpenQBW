//! `SYSOBJECT` catalog row parser (Phase 6, WP-6Z.2).
//!
//! Phase 6 (WP-6A, WP-6Z) assumed [`crate::SysColumn::owner_object_id`] joined to
//! [`crate::SysTableEntry::data_root_page`]. That assumption is wrong:
//! the two fields are independent integer namespaces and match only
//! by coincidence on a small subset of tables (18 of 535 owners on
//! Rock Castle, 14-26 of 400-430 owners on B18/B21/B22).
//!
//! The correct bridge is the `SYSOBJECT` catalog, which stores rows of
//! the form
//!
//! ```text
//! <object_id u32 LE> <20 bytes type/metadata> <name_len u8> <name>
//! ```
//!
//! on `Extent` pages (a representative example is page 550 on Rock
//! Castle, with 71 slotted rows visible). `object_id` is the same
//! integer namespace as [`crate::SysColumn::owner_object_id`], so a
//! scan that finds `<object_id><20 bytes><name_len><name>` where
//! `name` is a known [`crate::SysTableEntry::name`] yields the bridge.
//!
//! The scan is conservative: it requires a structural offset of
//! exactly 24 bytes between the candidate `object_id` and the
//! `<name_len><name>` tuple, and it only accepts names already present
//! in the parsed `SYSTABLE` set. False positives are bounded by the
//! probability that a random 4-byte sequence coincidentally equals a
//! SYSCOLUMN owner and is exactly 24 bytes before a SYSTABLE name; in
//! practice this is rare enough that majority-vote disambiguation
//! suffices when an owner has multiple candidate names.

use std::collections::{HashMap, HashSet};

use opensqlany::{ApModel, PageStore};

use crate::error::Result;
use crate::scan::{ReadOptions, Scan, for_each_extent_page};
use crate::syscolumn::SysColumn;
use crate::systable::SysTableEntry;

/// Byte distance between the `object_id` u32 and the `<name_len>`
/// byte in a `SYSOBJECT` row. Determined empirically on RC page 550.
pub const SYSOBJECT_NAME_OFFSET: usize = 24;

const NAME_LEN_MIN: usize = 3;
const NAME_LEN_MAX: usize = 48;

fn looks_like_identifier(s: &[u8]) -> bool {
    if s.is_empty() {
        return false;
    }
    if !(s[0].is_ascii_alphabetic() || s[0] == b'_') {
        return false;
    }
    s.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Scan all `Extent` pages for `SYSOBJECT`-style rows and return a map
/// from [`SysColumn::owner_object_id`] to [`SysTableEntry::name`].
///
/// The scan is conservative (see module docs); when an owner has several
/// candidate names the one with strictly the most sightings is chosen, and
/// a tie bridges nothing. Pages that fail to decode are rejected per
/// `options`; the second value counts those a lenient read skipped.
pub(crate) fn bridge_owners_to_tables(
    store: &PageStore,
    model: &ApModel,
    columns: &[SysColumn],
    tables: &[SysTableEntry],
    options: &ReadOptions,
) -> Result<(HashMap<u32, String>, usize)> {
    let owner_set: HashSet<u32> = columns.iter().map(|c| c.owner_object_id).collect();
    let mut name_set: HashSet<&str> = HashSet::new();
    for t in tables {
        name_set.insert(t.name.as_str());
    }
    if owner_set.is_empty() || name_set.is_empty() {
        return Ok((HashMap::new(), 0));
    }

    // owner -> name -> votes
    let mut votes: HashMap<u32, HashMap<String, u32>> = HashMap::new();

    let mut pages: Scan<()> = Scan::default();
    for_each_extent_page(
        store,
        model,
        options,
        &mut pages,
        |_| true,
        |decoded, _| {
            let plain = decoded.bytes();
            let mut i = SYSOBJECT_NAME_OFFSET;
            while i + 1 < plain.len() {
                let nlen = plain[i] as usize;
                if (NAME_LEN_MIN..=NAME_LEN_MAX).contains(&nlen) && i + 1 + nlen <= plain.len() {
                    let s = &plain[i + 1..i + 1 + nlen];
                    if looks_like_identifier(s)
                        && let Ok(name) = std::str::from_utf8(s)
                        && name_set.contains(name)
                    {
                        let oid_pos = i - SYSOBJECT_NAME_OFFSET;
                        let oid = u32::from_le_bytes([
                            plain[oid_pos],
                            plain[oid_pos + 1],
                            plain[oid_pos + 2],
                            plain[oid_pos + 3],
                        ]);
                        if owner_set.contains(&oid) {
                            *votes
                                .entry(oid)
                                .or_default()
                                .entry(name.to_string())
                                .or_insert(0) += 1;
                        }
                    }
                }
                i += 1;
            }
            Ok(())
        },
    )?;

    let mut bridge: HashMap<u32, String> = HashMap::new();
    for (oid, cands) in votes {
        if let Some(name) = winning_name(cands) {
            bridge.insert(oid, name);
        }
    }
    Ok((bridge, pages.rejected.len()))
}

/// The name with strictly the most votes. A tie bridges nothing: picking one
/// would depend on hash order and could name the wrong table.
fn winning_name(cands: HashMap<String, u32>) -> Option<String> {
    let best = cands.values().copied().max()?;
    let mut top = cands.into_iter().filter(|(_, c)| *c == best);
    let (name, _) = top.next()?;
    top.next().is_none().then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tied_names_bridge_nothing() {
        let tied: HashMap<String, u32> =
            [("a_table".to_owned(), 3), ("b_table".to_owned(), 3)].into();
        assert_eq!(winning_name(tied), None);
        let clear: HashMap<String, u32> =
            [("a_table".to_owned(), 3), ("b_table".to_owned(), 2)].into();
        assert_eq!(winning_name(clear).as_deref(), Some("a_table"));
    }

    #[test]
    fn identifier_check_basic() {
        assert!(looks_like_identifier(b"abmc_invoice_header"));
        assert!(looks_like_identifier(b"_underscore_lead"));
        assert!(!looks_like_identifier(b""));
        assert!(!looks_like_identifier(b"1starts_digit"));
        assert!(!looks_like_identifier(b"has space"));
        assert!(!looks_like_identifier(b"has-dash"));
    }
}
