//! `SYSINDEX` catalog row parser and attribution cross-validator (Phase 6, WP-6Z.3).
//!
//! The SA17 `SYSINDEX` system catalog stores one row per index in the
//! database. Each row identifies the index's owning table and the page
//! that holds the root of the index B-tree. The Python prototype in
//! `re/sysindex_scan.py` (C.30) reverse-engineered the layout:
//!
//! ```text
//! [2B flags] [2B creator = 01 46] [4B root u32_LE]
//! [4B zeros] [4B table_id u32_LE] [4B zeros]
//! [1B nlen]  [nlen bytes ASCII name]
//! ```
//!
//! `root_page` points at an `Extent` (E-type) page; `table_id` joins
//! back into [`crate::SysTableEntry::table_id`]. Index names that start
//! with `fkey_` correspond to foreign-key indexes (the dominant kind on
//! QuickBooks files); other names are primary-key or secondary indexes
//! managed by the engine.
//!
//! # Cross-validating page attribution
//!
//! `SYSINDEX.root_page` is recorded by the engine itself, so it is the
//! most reliable "ground truth" we have for "page P belongs to table T":
//! whatever table owns the index, that table's data pages are nearby
//! (root pages live in the same dbspace extent as the table data, per
//! C.30/C.32 on Rock Castle).
//!
//! [`CrossValidation::run`] uses this to audit the position-heuristic
//! [`crate::PageAttribution`]: for every distinct
//! `(table_id, root_page)` pair recovered from SYSINDEX, it checks
//! whether the position heuristic attributes `root_page` to a table
//! with the matching `table_id`. Disagreements indicate cases where
//! the position heuristic's "nearest-lower data_root" rule places an
//! index root inside a neighbouring table's window. The audit yields
//! per-table agreement counts that can be reported by `openqbw verify`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType, Result as SaResult};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_any};
use crate::page_attribution::PageAttribution;
use crate::systable::SysTableEntry;

/// Two-byte creator id appearing at row offset +2 of every SYSINDEX row.
/// Empirically constant across all QBW files inspected so far (C.30).
pub const SYSINDEX_CREATOR: [u8; 2] = [0x01, 0x46];

const NAME_LEN_MIN: usize = 1;
const NAME_LEN_MAX: usize = 128;
const PREAMBLE_LEN: usize = 22; // 2 flags + 2 creator + 4 root + 4 z + 4 tid + 4 z + 1 nlen + 1 first name byte
const PAGE_DATA_END: usize = 0xFF0;

/// One parsed `SYSINDEX` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysIndexEntry {
    /// Owning table id (joins to [`SysTableEntry::table_id`]).
    pub table_id: u32,
    /// Root page of the index B-tree (an `Extent` page).
    pub root_page: u32,
    /// Index name (ASCII).
    pub name: String,
    /// Page on which this row was found.
    pub page_number: u64,
    /// Byte offset of the row preamble within the decoded page body.
    pub row_offset: usize,
}

impl SysIndexEntry {
    /// True when the index name follows the `fkey_*` convention used
    /// by SA17 for foreign-key indexes.
    pub fn is_foreign_key(&self) -> bool {
        self.name.starts_with("fkey_")
    }
}

fn is_printable_ascii(b: u8) -> bool {
    (0x20..0x7F).contains(&b)
}

/// Scan a single decoded page body for `SYSINDEX` rows.
///
/// Matches occurrences of `?? ?? 01 46 <root> 0000 0000 <tid> 0000 0000
/// <nlen> <name>` where `root` and `tid` are non-zero, `nlen` is in
/// [`NAME_LEN_MIN`]`..=`[`NAME_LEN_MAX`], and `name` is printable ASCII.
pub fn scan_page(body: &[u8], pn: u64, out: &mut Vec<SysIndexEntry>) {
    let end = body.len().min(PAGE_DATA_END);
    if end < PREAMBLE_LEN {
        return;
    }
    let mut pos = 0usize;
    let limit = end - PREAMBLE_LEN;
    while pos <= limit {
        // Match creator at +2.
        if body[pos + 2] != SYSINDEX_CREATOR[0] || body[pos + 3] != SYSINDEX_CREATOR[1] {
            pos += 1;
            continue;
        }
        // Zero gaps at +8..+11 and +16..+19.
        if body[pos + 8] != 0
            || body[pos + 9] != 0
            || body[pos + 10] != 0
            || body[pos + 11] != 0
            || body[pos + 16] != 0
            || body[pos + 17] != 0
            || body[pos + 18] != 0
            || body[pos + 19] != 0
        {
            pos += 1;
            continue;
        }
        let root = u32::from_le_bytes([body[pos + 4], body[pos + 5], body[pos + 6], body[pos + 7]]);
        let tid = u32::from_le_bytes([
            body[pos + 12],
            body[pos + 13],
            body[pos + 14],
            body[pos + 15],
        ]);
        if root == 0 || tid == 0 {
            pos += 1;
            continue;
        }
        let nlen = body[pos + 20] as usize;
        if !(NAME_LEN_MIN..=NAME_LEN_MAX).contains(&nlen) {
            pos += 1;
            continue;
        }
        let name_start = pos + 21;
        let name_end = name_start + nlen;
        if name_end > end {
            pos += 1;
            continue;
        }
        let name_bytes = &body[name_start..name_end];
        if !name_bytes.iter().copied().all(is_printable_ascii) {
            pos += 1;
            continue;
        }
        let name = std::str::from_utf8(name_bytes)
            .expect("name guarded by printable-ASCII check")
            .to_owned();
        out.push(SysIndexEntry {
            table_id: tid,
            root_page: root,
            name,
            page_number: pn,
            row_offset: pos,
        });
        pos = name_end;
    }
}

/// Iterate every `SYSINDEX` row recovered from `store`.
///
/// Mirrors the bv-recovery strategy used by [`crate::iter_systable_entries`]
/// and [`crate::iter_syscolumns`].
pub fn iter_sysindex<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = SysIndexEntry> + 'a {
    SysIndexIter::new(store, model)
}

/// Collect a deduplicated set of SYSINDEX entries keyed by
/// `(table_id, root_page, name)`, preferring the first sighting.
pub fn collect_unique(store: &PageStore, model: &ApModel) -> Vec<SysIndexEntry> {
    let mut uniq: BTreeMap<(u32, u32, String), SysIndexEntry> = BTreeMap::new();
    for e in iter_sysindex(store, model) {
        uniq.entry((e.table_id, e.root_page, e.name.clone()))
            .or_insert(e);
    }
    uniq.into_values().collect()
}

struct SysIndexIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    pn: u64,
    n_pages: u64,
    buffer: Vec<SysIndexEntry>,
}

impl<'a> SysIndexIter<'a> {
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
            for e in found.into_iter().rev() {
                self.buffer.push(e);
            }
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for SysIndexIter<'_> {
    type Item = SysIndexEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(e) = self.buffer.pop() {
                return Some(e);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                _ => return None,
            }
        }
    }
}

impl FusedIterator for SysIndexIter<'_> {}

/// Outcome of auditing one SYSINDEX `(root_page, table_id)` pair against
/// a position-heuristic [`PageAttribution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Position heuristic attributes `root_page` to the same table.
    Agree,
    /// Position heuristic attributes `root_page` to a *different* table
    /// than SYSINDEX records.
    Disagree,
    /// Position heuristic returns no attribution for `root_page`.
    Missing,
    /// SYSINDEX's `table_id` does not appear in the SYSTABLE catalog
    /// (orphaned index row, usually a catalog index for an internal
    /// table).
    OrphanIndex,
}

/// Per-table-id agreement counts.
#[derive(Debug, Clone, Default)]
pub struct CrossValidation {
    /// Total SYSINDEX rows audited.
    pub total: usize,
    /// Distinct (table_id, root_page) pairs audited (deduplicated input).
    pub distinct_roots: usize,
    /// Pairs where the position heuristic agreed.
    pub agree: usize,
    /// Pairs where the position heuristic placed the root in a
    /// different table.
    pub disagree: usize,
    /// Pairs where the position heuristic returned no attribution.
    pub missing: usize,
    /// Pairs whose `table_id` could not be resolved against SYSTABLE.
    pub orphan_index: usize,
    /// First few disagreements, kept for diagnostic printing.
    /// Tuple is `(table_id, sysindex_table_name, position_table_name, root_page, index_name)`.
    pub disagree_samples: Vec<(u32, String, String, u32, String)>,
}

/// Maximum disagreement samples retained in [`CrossValidation::disagree_samples`].
pub const DISAGREE_SAMPLE_LIMIT: usize = 16;

impl CrossValidation {
    /// Audit the given `entries` against the position-heuristic
    /// attribution `position`, resolving `table_id` to names via the
    /// `tables` catalog.
    pub fn run(
        entries: &[SysIndexEntry],
        position: &PageAttribution,
        tables: &[SysTableEntry],
    ) -> Self {
        let mut name_by_tid: HashMap<u32, String> = HashMap::new();
        for t in tables {
            name_by_tid
                .entry(t.table_id)
                .or_insert_with(|| t.name.clone());
        }
        let mut seen: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut stats = CrossValidation {
            total: entries.len(),
            ..Default::default()
        };
        for e in entries {
            if !seen.insert((e.table_id, e.root_page)) {
                continue;
            }
            stats.distinct_roots += 1;
            let sysindex_name = name_by_tid.get(&e.table_id).cloned();
            let pos_name = position
                .attribute(u64::from(e.root_page))
                .map(|t| t.name.clone());
            let outcome = classify(sysindex_name.as_deref(), pos_name.as_deref());
            match outcome {
                AuditOutcome::Agree => stats.agree += 1,
                AuditOutcome::Disagree => {
                    stats.disagree += 1;
                    if stats.disagree_samples.len() < DISAGREE_SAMPLE_LIMIT {
                        stats.disagree_samples.push((
                            e.table_id,
                            sysindex_name.unwrap_or_else(|| "<unknown>".into()),
                            pos_name.unwrap_or_else(|| "<none>".into()),
                            e.root_page,
                            e.name.clone(),
                        ));
                    }
                }
                AuditOutcome::Missing => stats.missing += 1,
                AuditOutcome::OrphanIndex => stats.orphan_index += 1,
            }
        }
        stats
    }

    /// Agreement rate over distinct (table_id, root_page) pairs that
    /// have a known SYSTABLE table_id (i.e. excluding orphans).
    pub fn agreement_rate(&self) -> f64 {
        let resolvable = self.agree + self.disagree + self.missing;
        if resolvable == 0 {
            0.0
        } else {
            self.agree as f64 / resolvable as f64
        }
    }
}

fn classify(sysindex_name: Option<&str>, pos_name: Option<&str>) -> AuditOutcome {
    match (sysindex_name, pos_name) {
        (None, _) => AuditOutcome::OrphanIndex,
        (Some(_), None) => AuditOutcome::Missing,
        (Some(a), Some(b)) if a == b => AuditOutcome::Agree,
        (Some(_), Some(_)) => AuditOutcome::Disagree,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic SYSINDEX row preamble + name.
    fn synth_row(tid: u32, root: u32, name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0x00, 0x00]); // flags
        v.extend_from_slice(&SYSINDEX_CREATOR);
        v.extend_from_slice(&root.to_le_bytes());
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&tid.to_le_bytes());
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        v.push(name.len() as u8);
        v.extend_from_slice(name.as_bytes());
        v
    }

    fn table(tid: u32, name: &str, root: u32, last: u32) -> SysTableEntry {
        SysTableEntry {
            table_id: tid,
            name: name.into(),
            magic: [0; 4],
            col_count: None,
            data_root_page: Some(root),
            last_page: Some(last),
            page_number: 0,
            row_offset: 0,
        }
    }

    #[test]
    fn scan_finds_single_row() {
        let mut body = vec![0u8; 0x200];
        let row = synth_row(5887, 8418, "fkey_invoice_customer");
        body[0x40..0x40 + row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 99, &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.table_id, 5887);
        assert_eq!(e.root_page, 8418);
        assert_eq!(e.name, "fkey_invoice_customer");
        assert_eq!(e.page_number, 99);
        assert_eq!(e.row_offset, 0x40);
        assert!(e.is_foreign_key());
    }

    #[test]
    fn scan_rejects_zero_root_or_zero_tid() {
        let mut body = vec![0u8; 0x100];
        let row = synth_row(0, 100, "bogus");
        body[0..row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert!(out.is_empty());

        let mut body2 = vec![0u8; 0x100];
        let row2 = synth_row(5, 0, "bogus2");
        body2[0..row2.len()].copy_from_slice(&row2);
        let mut out2 = Vec::new();
        scan_page(&body2, 0, &mut out2);
        assert!(out2.is_empty());
    }

    #[test]
    fn scan_rejects_non_ascii_name() {
        let mut body = vec![0u8; 0x100];
        let mut row = synth_row(5, 100, "x");
        // Replace the printable 'x' with a non-printable byte.
        let last = row.len() - 1;
        row[last] = 0x01;
        body[0..row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_emits_multiple_back_to_back() {
        let mut body = vec![0u8; 0x300];
        let r1 = synth_row(1, 100, "pk_a");
        let r2 = synth_row(2, 200, "fkey_b_a");
        body[0x10..0x10 + r1.len()].copy_from_slice(&r1);
        body[0x80..0x80 + r2.len()].copy_from_slice(&r2);
        let mut out = Vec::new();
        scan_page(&body, 1, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "pk_a");
        assert_eq!(out[1].name, "fkey_b_a");
        assert!(!out[0].is_foreign_key());
        assert!(out[1].is_foreign_key());
    }

    #[test]
    fn cross_validation_agree_disagree_missing_orphan() {
        let tables = vec![table(10, "alpha", 100, 200), table(20, "beta", 300, 400)];
        let position = PageAttribution::from_catalog(tables.clone());
        let entries = vec![
            // Agree: tid=10 root=150 (alpha's window).
            SysIndexEntry {
                table_id: 10,
                root_page: 150,
                name: "pk".into(),
                page_number: 0,
                row_offset: 0,
            },
            // Disagree: tid=10 root=350 (in beta's window).
            SysIndexEntry {
                table_id: 10,
                root_page: 350,
                name: "fkey_x".into(),
                page_number: 0,
                row_offset: 0,
            },
            // Missing: tid=20 root=500 (beyond beta's last_page).
            SysIndexEntry {
                table_id: 20,
                root_page: 500,
                name: "pk".into(),
                page_number: 0,
                row_offset: 0,
            },
            // Orphan: tid=99 (unknown table).
            SysIndexEntry {
                table_id: 99,
                root_page: 150,
                name: "pk".into(),
                page_number: 0,
                row_offset: 0,
            },
            // Duplicate of the first agree row (should be dedup-counted).
            SysIndexEntry {
                table_id: 10,
                root_page: 150,
                name: "pk".into(),
                page_number: 0,
                row_offset: 0,
            },
        ];
        let v = CrossValidation::run(&entries, &position, &tables);
        assert_eq!(v.total, 5);
        assert_eq!(v.distinct_roots, 4);
        assert_eq!(v.agree, 1);
        assert_eq!(v.disagree, 1);
        assert_eq!(v.missing, 1);
        assert_eq!(v.orphan_index, 1);
        let resolvable = v.agree + v.disagree + v.missing;
        assert_eq!(resolvable, 3);
        assert!((v.agreement_rate() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(v.disagree_samples.len(), 1);
        let (tid, sn, pn, root, idx) = &v.disagree_samples[0];
        assert_eq!(*tid, 10);
        assert_eq!(sn, "alpha");
        assert_eq!(pn, "beta");
        assert_eq!(*root, 350);
        assert_eq!(idx, "fkey_x");
    }

    #[test]
    fn classify_orphan_when_table_unknown_even_with_position() {
        assert_eq!(classify(None, Some("alpha")), AuditOutcome::OrphanIndex);
        assert_eq!(classify(None, None), AuditOutcome::OrphanIndex);
    }
}
