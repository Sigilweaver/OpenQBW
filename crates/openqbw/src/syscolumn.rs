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
//! <owner_object_id u32 LE>                 -- SA17 table id; bridged to
//!                                          --   a table name via the
//!                                          --   SYSTABLE row prefix
//!                                          --   (see [`crate::owner_bridge`]).
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
//! Owners are bridged to user-visible table names through the table-id
//! back-reference in the `SYSTABLE` row prefix (Phase 6, WP-6Z.4; see
//! [`crate::owner_bridge`]), falling back to the `SYSOBJECT` catalog
//! scan (WP-6Z.2, [`crate::sysobject`]) for owners it does not cover.
//! The earlier WP-6A attempt to join [`SysColumn::owner_object_id`]
//! against [`crate::SysTableEntry::data_root_page`] was wrong: the two
//! are independent integer namespaces (see `re/NOTES.md` C.52).

use std::collections::{BTreeMap, BTreeSet};
use std::iter::FusedIterator;

use opensqlany::{ApModel, Page, PageStore, PageType, Result as SaResult, SlottedPage};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_any};
use crate::owner_bridge::BridgeSource;

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
    /// SA17 table id. Bridged to a table name via the `SYSTABLE` row
    /// prefix (see [`crate::owner_bridge`]).
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
            if !s.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_') {
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
        if body[i..i + SYSCOLUMN_TAG.len()] != SYSCOLUMN_TAG {
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
        let col_id = u32::from_le_bytes([body[p + 4], body[p + 5], body[p + 6], body[p + 7]]);
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

/// Largest `column_id` treated as a real ordinal rather than the
/// residue of a mis-parsed row.
const MAX_PLAUSIBLE_COLUMN_ID: u32 = 4096;

/// Everything [`recover_schema`] learned while looking a table up, so a
/// caller can explain a miss instead of reporting a bare "not found"
/// (openqbw#19).
#[derive(Debug, Clone)]
pub struct SchemaRecovery {
    /// Table that was asked for.
    pub table: String,
    /// Whether the `SYSTABLE` catalog lists that name at all.
    pub in_catalog: bool,
    /// `SysTableEntry::table_id` of the first catalog row for the name.
    pub catalog_table_id: Option<u32>,
    /// Total `SYSCOLUMN` rows recovered from the file.
    pub syscolumn_rows: usize,
    /// Distinct `owner_object_id` values among those rows.
    pub distinct_owners: usize,
    /// Owner the table was bridged to, if any.
    pub owner: Option<u32>,
    /// How that owner was bridged.
    pub source: Option<BridgeSource>,
    /// Back-reference distance calibrated for this file.
    pub backref_distance: Option<usize>,
    /// Distinct table names the bridge covers.
    pub bridged_tables: usize,
    /// Owners bridged by the `SYSTABLE` back-reference.
    pub from_backref: usize,
    /// Owners bridged by the `SYSOBJECT` scan.
    pub from_sysobject: usize,
    /// The table's columns, ordered by `column_id`. Empty when the table
    /// could not be bridged to an owner.
    pub columns: Vec<SysColumn>,
}

impl SchemaRecovery {
    /// Columns whose `column_id` is a plausible ordinal. A mis-parsed
    /// row can carry a wild id (`786432` has been seen on
    /// `B22_Sample.qbw`), which would otherwise swamp the gap report.
    pub fn plausible_columns(&self) -> impl DoubleEndedIterator<Item = &SysColumn> {
        self.columns
            .iter()
            .filter(|c| c.column_id > 0 && c.column_id <= MAX_PLAUSIBLE_COLUMN_ID)
    }

    /// Number of recovered rows whose `column_id` is not a plausible
    /// ordinal, i.e. rows this file decoded badly.
    pub fn implausible_column_ids(&self) -> usize {
        self.columns.len() - self.plausible_columns().count()
    }

    /// Lowest and highest plausible `column_id` recovered, if any.
    pub fn column_id_range(&self) -> Option<(u32, u32)> {
        let first = self.plausible_columns().next()?.column_id;
        let last = self.plausible_columns().next_back()?.column_id;
        Some((first, last))
    }

    /// How many `column_id`s inside [`Self::column_id_range`] have no
    /// recovered row, i.e. how many of the table's `SYSCOLUMN` rows are
    /// still missing.
    pub fn missing_column_ids(&self) -> usize {
        match self.column_id_range() {
            Some((first, last)) => {
                // Different recovered names can share an ordinal. Count each
                // occupied position once, rather than subtracting row count.
                let recovered_ids: BTreeSet<u32> =
                    self.plausible_columns().map(|c| c.column_id).collect();
                (last - first) as usize + 1 - recovered_ids.len()
            }
            None => 0,
        }
    }

    /// `true` when the recovered `column_id`s have holes.
    pub fn has_column_gaps(&self) -> bool {
        self.missing_column_ids() > 0
    }
}

/// Return all columns for the table named `table_name`, ordered by
/// `column_id`.
///
/// Returns an empty vector if the table cannot be bridged to a
/// SYSCOLUMN owner. Use [`recover_schema`] to find out why.
pub fn schema_for(store: &PageStore, model: &ApModel, table_name: &str) -> Vec<SysColumn> {
    recover_schema(store, model, table_name).columns
}

/// Look a table's columns up, reporting what the lookup found on the way.
///
/// Owners are bridged to table names by the `SYSTABLE` back-reference
/// ([`crate::owner_bridge`]), falling back to the whole-file `SYSOBJECT`
/// name scan ([`crate::sysobject`]) only when the back-reference does not
/// resolve the requested table - the fallback costs a full pass over the
/// file and, on the files measured so far, adds little the
/// back-reference has not already covered.
pub fn recover_schema(store: &PageStore, model: &ApModel, table_name: &str) -> SchemaRecovery {
    let columns: Vec<SysColumn> = iter_syscolumns(store, model).collect();
    let tables = crate::iter_systable_entries(store, model).collect::<Vec<_>>();

    let catalog_row = tables.iter().find(|t| t.name == table_name);
    let mut out = SchemaRecovery {
        table: table_name.to_string(),
        in_catalog: catalog_row.is_some(),
        catalog_table_id: catalog_row.map(|t| t.table_id),
        syscolumn_rows: columns.len(),
        distinct_owners: columns
            .iter()
            .map(|c| c.owner_object_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        owner: None,
        source: None,
        backref_distance: None,
        bridged_tables: 0,
        from_backref: 0,
        from_sysobject: 0,
        columns: Vec::new(),
    };

    let mut bridge =
        crate::owner_bridge::bridge_owners_via_backref(store, model, &columns, &tables);
    if bridge.owners_for(table_name).is_empty() {
        crate::owner_bridge::extend_with_sysobject(&mut bridge, store, model, &columns, &tables);
    }
    out.backref_distance = bridge.backref_distance;
    out.bridged_tables = bridge.tables();
    out.from_backref = bridge.from_backref;
    out.from_sysobject = bridge.from_sysobject;

    // A name can attract more than one owner; take the owner with the
    // most recovered columns, lowest id first for determinism.
    let Some(owner) = bridge
        .owners_for(table_name)
        .into_iter()
        .max_by_key(|owner| {
            let n = columns
                .iter()
                .filter(|c| c.owner_object_id == *owner)
                .map(|c| (c.column_id, c.name.as_str()))
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            (n, std::cmp::Reverse(*owner))
        })
    else {
        return out;
    };
    out.owner = Some(owner);
    out.source = bridge.source_for(owner);

    let mut cols: Vec<SysColumn> = columns
        .into_iter()
        .filter(|c| c.owner_object_id == owner)
        .collect();
    cols.sort_by_key(|c| c.column_id);
    cols.dedup_by(|a, b| a.column_id == b.column_id && a.name == b.name);
    out.columns = cols;
    out
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
            let plain = if let Some(bv) = recover_bv_any(pn, raw) {
                deobfuscate_with_bv(raw, pn, bv)
            } else {
                self.model.deobfuscate_with_store(raw, pn, self.store)
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
    #[allow(clippy::too_many_arguments)]
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

    fn recovery_with_ids(ids: &[u32]) -> SchemaRecovery {
        SchemaRecovery {
            table: "abmc_invoice_header".into(),
            in_catalog: true,
            catalog_table_id: Some(5884),
            syscolumn_rows: 5419,
            distinct_owners: 679,
            owner: Some(3073),
            source: Some(BridgeSource::SysTableBackref),
            backref_distance: Some(30),
            bridged_tables: 422,
            from_backref: 421,
            from_sysobject: 0,
            columns: ids
                .iter()
                .map(|id| SysColumn {
                    name: format!("c{id}"),
                    owner_object_id: 3073,
                    column_id: *id,
                    nulls_flag: 0,
                    domain_char: b'N',
                    width: 4,
                    page_number: 710,
                    tag_offset: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn contiguous_columns_report_no_gaps() {
        let r = recovery_with_ids(&[1, 2, 3, 4]);
        assert_eq!(r.column_id_range(), Some((1, 4)));
        assert_eq!(r.missing_column_ids(), 0);
        assert!(!r.has_column_gaps());
    }

    #[test]
    fn holes_in_the_column_ids_are_counted() {
        let r = recovery_with_ids(&[1, 2, 5, 9]);
        assert_eq!(r.column_id_range(), Some((1, 9)));
        assert_eq!(r.missing_column_ids(), 5);
        assert!(r.has_column_gaps());
    }

    #[test]
    fn duplicate_ordinal_names_do_not_underflow_gap_count() {
        let mut r = recovery_with_ids(&[1, 1]);
        r.columns[1].name = "different_name".into();
        assert_eq!(r.column_id_range(), Some((1, 1)));
        assert_eq!(r.missing_column_ids(), 0);
        assert!(!r.has_column_gaps());
    }

    #[test]
    fn duplicate_ordinal_names_do_not_hide_a_missing_position() {
        let mut r = recovery_with_ids(&[1, 1, 3]);
        r.columns[1].name = "different_name".into();
        assert_eq!(r.column_id_range(), Some((1, 3)));
        assert_eq!(r.missing_column_ids(), 1);
        assert!(r.has_column_gaps());
    }

    #[test]
    fn a_wild_column_id_does_not_swamp_the_gap_report() {
        // Seen on B22_Sample.qbw: one mis-parsed row claims id 786432.
        let r = recovery_with_ids(&[1, 2, 3, 786_432]);
        assert_eq!(r.column_id_range(), Some((1, 3)));
        assert_eq!(r.missing_column_ids(), 0);
        assert_eq!(r.implausible_column_ids(), 1);
    }
}
