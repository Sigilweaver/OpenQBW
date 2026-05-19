//! `SYSOBJECT` catalog row parser (Phase 7, WP-7A).
//!
//! Phase 6 assumed [`crate::SysColumn::owner_object_id`] joined to
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

use opensqlany::{ApModel, PageStore, PageType};

use crate::bv_recovery::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};
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
/// `columns` and `tables` should come from
/// [`crate::iter_syscolumns`] and [`crate::iter_systable_entries`]
/// respectively (or their deduplicated forms). The scan is conservative
/// (see module docs); when an owner has multiple candidate names the
/// one with the highest sighting count is chosen.
pub fn bridge_owners_to_tables(
    store: &PageStore,
    model: &ApModel,
    columns: &[SysColumn],
    tables: &[SysTableEntry],
) -> HashMap<u32, String> {
    let owner_set: HashSet<u32> = columns.iter().map(|c| c.owner_object_id).collect();
    let mut name_set: HashSet<&str> = HashSet::new();
    for t in tables {
        name_set.insert(t.name.as_str());
    }
    if owner_set.is_empty() || name_set.is_empty() {
        return HashMap::new();
    }

    // owner -> name -> votes
    let mut votes: HashMap<u32, HashMap<String, u32>> = HashMap::new();

    let n_pages = store.page_count();
    for pn in 0..n_pages {
        let Ok(page) = store.page(pn) else { continue };
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        let raw = page.bytes();
        let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
            deobfuscate_with_bv(raw, pn, bv)
        } else {
            let bv = oracle_bv_e_page(pn, raw);
            let cand = deobfuscate_with_bv(raw, pn, bv);
            if cand[0] == 0 {
                cand
            } else {
                model.deobfuscate_with_store(raw, pn, store)
            }
        };

        let mut i = SYSOBJECT_NAME_OFFSET;
        while i + 1 < plain.len() {
            let nlen = plain[i] as usize;
            if (NAME_LEN_MIN..=NAME_LEN_MAX).contains(&nlen)
                && i + 1 + nlen <= plain.len()
            {
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
    }

    let mut bridge: HashMap<u32, String> = HashMap::new();
    for (oid, cands) in votes {
        if let Some((name, _)) = cands.into_iter().max_by_key(|(_, c)| *c) {
            bridge.insert(oid, name);
        }
    }
    bridge
}

#[cfg(test)]
mod tests {
    use super::*;

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
