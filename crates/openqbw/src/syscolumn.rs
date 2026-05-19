//! `SYSCOLUMN` catalog row parser (Phase 6, WP-6A).
//!
//! Each `SYSCOLUMN` row stores one column definition. On Rock Castle the
//! body has the layout (reverse-engineered in `re/NOTES.md`):
//!
//! ```text
//! <name_len u8> <name>
//! [<default_len u8> <default>]?           -- optional, may be absent
//! 01 52 00 01 00 00 00 00                  -- 8-byte fixed tag
//! <row_id u32 LE>                          -- per-row id (ignored)
//! <owner_object_id u32 LE>                 -- SA17 object_id; bridged
//!                                          --   to a table name via the
//!                                          --   SYSOBJECT catalog
//!                                          --   (see [`crate::sysobject`]).
//! <column_id u32 LE>                       -- ordinal within the table
//! <nulls_flag u8> <pad u8>
//! 01 <domain_char u8> <width u8>
//! ```
//!
//! The leading name is recovered by walking backwards from the tag and
//! trying name lengths 3..=40, optionally peeling a trailing default-value
//! length-prefixed string. The first match whose declared length byte sits
//! immediately before a printable identifier wins.
//!
//! Owners are bridged to user-visible table names through the
//! `SYSOBJECT` catalog (Phase 7, WP-7A). The earlier Phase 6 attempt
//! to join [`SysColumn::owner_object_id`] against
//! [`crate::SysTableEntry::data_root_page`] was wrong: the two are
//! independent integer namespaces (see `re/NOTES.md` C.49).

use std::collections::BTreeMap;
use std::iter::FusedIterator;

use opensqlany::{ApModel, Page, PageStore, PageType, Result as SaResult, SlottedPage};

use crate::bv_recovery::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};

/// Fixed 8-byte anchor that precedes the numeric portion of every
/// `SYSCOLUMN` row body.
pub const SYSCOLUMN_TAG: [u8; 8] = [0x01, 0x52, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00];

const NAME_LEN_MIN: usize = 1;
const NAME_LEN_MAX: usize = 40;
/// Possible byte gaps between the column name and the [`SYSCOLUMN_TAG`].
/// A non-zero peel skips over an optional default-value length-prefixed
/// string sitting between the name and the tag.
const DEFAULT_PEELS: [usize; 7] = [0, 1, 2, 3, 4, 8, 16];

/// One parsed `SYSCOLUMN` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysColumn {
    /// Column name (ASCII identifier).
    pub name: String,
    /// SA17 object_id. Bridged to a table name via the `SYSOBJECT`
    /// catalog (see [`crate::sysobject`]).
    pub owner_object_id: u32,
    /// Ordinal position of this column inside its owning table.
    pub column_id: u32,
    /// Nullability/flags byte (semantics RE-pending; see WP-6D).
    pub nulls_flag: u8,
    /// Single-character SA17 domain code (e.g. `N` = unsigned int, `Y` =
    /// signed/varchar). Other codes are surfaced verbatim for diagnostics.
    pub domain_char: u8,
    /// Declared column width / precision byte (units depend on `domain_char`).
    pub width: u8,
    /// Page on which this row was found.
    pub page_number: u64,
    /// Byte offset of the tag within the decoded page body.
    pub tag_offset: usize,
}

/// Walk back from `tag_pos` and try to locate a length-prefixed ASCII
/// identifier that ends immediately before the tag, optionally separated
/// from it by a default-value length-prefixed string.
fn find_name_before(body: &[u8], tag_pos: usize) -> Option<String> {
    for peel in DEFAULT_PEELS {
        if tag_pos < peel + NAME_LEN_MIN + 1 {
            continue;
        }
        let inner = tag_pos - peel;
        for name_len in NAME_LEN_MIN..=NAME_LEN_MAX {
            if inner < name_len + 1 {
                continue;
            }
            let len_off = inner - name_len - 1;
            if body[len_off] as usize != name_len {
                continue;
            }
            let s = &body[len_off + 1..len_off + 1 + name_len];
            if !s
                .iter()
                .all(|&b| b.is_ascii_alphanumeric() || b == b'_')
            {
                continue;
            }
            if !(s[0].is_ascii_alphabetic() || s[0] == b'_') {
                continue;
            }
            return Some(s.iter().map(|&b| b as char).collect());
        }
    }
    None
}

/// Parse all `SYSCOLUMN` rows out of a single slotted-page row body.
fn parse_rows_in_body(body: &[u8], pn: u64, out: &mut Vec<SysColumn>) {
    let n = body.len();
    if n < SYSCOLUMN_TAG.len() + 17 {
        return;
    }
    let mut i = 0usize;
    while i + SYSCOLUMN_TAG.len() + 17 <= n {
        if &body[i..i + SYSCOLUMN_TAG.len()] != &SYSCOLUMN_TAG {
            i += 1;
            continue;
        }
        let Some(name) = find_name_before(body, i) else {
            i += SYSCOLUMN_TAG.len();
            continue;
        };
        let p = i + SYSCOLUMN_TAG.len() + 4;
        if p + 9 > n {
            break;
        }
        let owner = u32::from_le_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
        let col_id = u32::from_le_bytes([
            body[p + 4],
            body[p + 5],
            body[p + 6],
            body[p + 7],
        ]);
        let nulls_flag = body[p + 8];
        if body[p + 10] != 0x01 {
            i += SYSCOLUMN_TAG.len();
            continue;
        }
        let domain_char = body[p + 11];
        let width = body[p + 12];
        if !domain_char.is_ascii_alphabetic() {
            i += SYSCOLUMN_TAG.len();
            continue;
        }
        out.push(SysColumn {
            name,
            owner_object_id: owner,
            column_id: col_id,
            nulls_flag,
            domain_char,
            width,
            page_number: pn,
            tag_offset: i,
        });
        i += SYSCOLUMN_TAG.len();
    }
}

/// Scan every slotted-page row body on a single decoded page for
/// `SYSCOLUMN` rows.
pub fn scan_page(plain: &[u8], pn: u64, out: &mut Vec<SysColumn>) {
    let page = Page::from_bytes(pn, plain);
    let sp = SlottedPage::parse(page);
    if sp.directory.is_none() {
        return;
    }
    for (_off, body) in sp.row_bytes() {
        parse_rows_in_body(body, pn, out);
    }
}

/// Iterate every `SYSCOLUMN` row recovered from `store`.
pub fn iter_syscolumns<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = SysColumn> + 'a {
    SysColumnIter::new(store, model)
}

/// Deduplicate `SYSCOLUMN` rows by `(owner_object_id, column_id, name)`
/// and return them ordered by `(owner_object_id, column_id)`.
pub fn collect_unique(store: &PageStore, model: &ApModel) -> Vec<SysColumn> {
    let mut uniq: BTreeMap<(u32, u32, String), SysColumn> = BTreeMap::new();
    for c in iter_syscolumns(store, model) {
        uniq.entry((c.owner_object_id, c.column_id, c.name.clone()))
            .or_insert(c);
    }
    uniq.into_values().collect()
}

/// Return all columns for the table named `table_name`, ordered by
/// `column_id`. The bridge is via the `SYSOBJECT` catalog
/// (see [`crate::sysobject::bridge_owners_to_tables`]).
///
/// Returns an empty vector if the table cannot be bridged to a
/// SYSCOLUMN owner.
pub fn schema_for(
    store: &PageStore,
    model: &ApModel,
    table_name: &str,
) -> Vec<SysColumn> {
    let columns: Vec<SysColumn> = iter_syscolumns(store, model).collect();
    let tables = crate::iter_systable_entries(store, model).collect::<Vec<_>>();
    let bridge = crate::sysobject::bridge_owners_to_tables(store, model, &columns, &tables);
    let Some((&owner, _)) = bridge.iter().find(|(_, n)| n.as_str() == table_name) else {
        return Vec::new();
    };
    let mut cols: Vec<SysColumn> = columns
        .into_iter()
        .filter(|c| c.owner_object_id == owner)
        .collect();
    cols.sort_by_key(|c| c.column_id);
    cols.dedup_by(|a, b| a.column_id == b.column_id && a.name == b.name);
    cols
}

struct SysColumnIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    pn: u64,
    n_pages: u64,
    buffer: Vec<SysColumn>,
}

impl<'a> SysColumnIter<'a> {
    fn new(store: &'a PageStore, model: &'a ApModel) -> Self {
        Self {
            store,
            model,
            pn: 1,
            n_pages: store.page_count(),
            buffer: Vec::new(),
        }
    }

    fn fill_buffer(&mut self) -> SaResult<bool> {
        while self.buffer.is_empty() && self.pn < self.n_pages {
            let pn = self.pn;
            self.pn += 1;
            let page = self.store.page(pn)?;
            if page.trailer().page_type() != PageType::Extent {
                continue;
            }
            let raw = page.bytes();
            let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
                deobfuscate_with_bv(raw, pn, bv)
            } else {
                let bv = oracle_bv_e_page(pn, raw);
                let candidate = deobfuscate_with_bv(raw, pn, bv);
                if candidate[0] == 0 {
                    candidate
                } else {
                    self.model.deobfuscate_with_store(raw, pn, self.store)
                }
            };
            let mut found = Vec::new();
            scan_page(&plain, pn, &mut found);
            for c in found.into_iter().rev() {
                self.buffer.push(c);
            }
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for SysColumnIter<'_> {
    type Item = SysColumn;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(c) = self.buffer.pop() {
                return Some(c);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                _ => return None,
            }
        }
    }
}

impl FusedIterator for SysColumnIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic SYSCOLUMN row body:
    ///   <name_len><name>[<def_len><def>]<tag><row_id><owner><col_id>
    ///   <nulls><pad>01<domain><width>
    fn synth_row(
        name: &str,
        default: Option<&str>,
        row_id: u32,
        owner: u32,
        col_id: u32,
        nulls: u8,
        domain: u8,
        width: u8,
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(name.len() as u8);
        v.extend_from_slice(name.as_bytes());
        if let Some(d) = default {
            v.push(d.len() as u8);
            v.extend_from_slice(d.as_bytes());
        }
        v.extend_from_slice(&SYSCOLUMN_TAG);
        v.extend_from_slice(&row_id.to_le_bytes());
        v.extend_from_slice(&owner.to_le_bytes());
        v.extend_from_slice(&col_id.to_le_bytes());
        v.push(nulls);
        v.push(0x00);
        v.push(0x01);
        v.push(domain);
        v.push(width);
        v
    }

    #[test]
    fn parses_single_row_without_default() {
        let body = synth_row("account_id", None, 0x80000001, 3680, 1, 2, b'N', 4);
        let mut out = Vec::new();
        parse_rows_in_body(&body, 42, &mut out);
        assert_eq!(out.len(), 1);
        let c = &out[0];
        assert_eq!(c.name, "account_id");
        assert_eq!(c.owner_object_id, 3680);
        assert_eq!(c.column_id, 1);
        assert_eq!(c.nulls_flag, 2);
        assert_eq!(c.domain_char, b'N');
        assert_eq!(c.width, 4);
        assert_eq!(c.page_number, 42);
    }

    #[test]
    fn parses_multiple_rows_concatenated() {
        let mut body = synth_row("amount_amt", None, 1, 100, 7, 2, b'Y', 8);
        body.extend(synth_row("memo", None, 2, 100, 8, 1, b'Y', 64));
        let mut out = Vec::new();
        parse_rows_in_body(&body, 0, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "amount_amt");
        assert_eq!(out[1].name, "memo");
        assert_eq!(out[1].width, 64);
    }

    #[test]
    fn handles_underscore_and_digits_in_name() {
        let body = synth_row("col_42_xy", None, 0, 5, 1, 0, b'N', 1);
        let mut out = Vec::new();
        parse_rows_in_body(&body, 0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "col_42_xy");
    }

    #[test]
    fn rejects_bad_marker() {
        let mut body = synth_row("good", None, 0, 1, 1, 0, b'N', 4);
        // Corrupt the 0x01 marker before domain.
        let mark = body.len() - 3;
        body[mark] = 0x00;
        let mut out = Vec::new();
        parse_rows_in_body(&body, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn rejects_non_alpha_domain() {
        let body = synth_row("col", None, 0, 1, 1, 0, 0xFF, 4);
        let mut out = Vec::new();
        parse_rows_in_body(&body, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn name_back_walk_skips_into_garbage_prefix() {
        // Garbage prefix followed by a valid row.
        let mut body = vec![0xAA, 0xBB, 0xCC, 0xDD];
        body.extend(synth_row("real_name", None, 0, 1, 1, 0, b'N', 4));
        let mut out = Vec::new();
        parse_rows_in_body(&body, 0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "real_name");
    }
}
