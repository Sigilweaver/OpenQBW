//! Schema-aware page attribution validator (Phase 6, WP-6Z).
//!
//! Prior phases established that there is no on-disk per-table tag in
//! SA17 page headers (WP-3A, WP-4C refuted) and no walkable per-table
//! B-tree of interior nodes (WP-4B refuted: I-pages are empty in QBW).
//! The position-based [`crate::page_attribution::PageAttribution`]
//! (C.47 nearest-lower-`data_root` heuristic) therefore remains the
//! only available attribution signal, and is known to be noisy
//! (line items disperse across 86 distinct `source_table` buckets on
//! Rock Castle).
//!
//! This module adds an orthogonal *validation* signal derived from the
//! SYSCOLUMN catalog (Phase 6 WP-6A). For each table we precompute a
//! plausible **row-width band** from the declared column widths and
//! domain codes, then check each candidate page against that band
//! using the median row body length from its slot directory.
//!
//! ## What it is and is not
//!
//! `SchemaAttribution` is a *validator*, not a re-attributer. Given a
//! page already attributed to table `T` by position, it answers
//! "does the page's row layout look like it belongs to `T`?". The
//! production line-item exporter is unchanged; downstream callers can
//! use the [`SchemaAttribution::validate`] method to flag suspicious
//! attributions and `--strict-attribution` consumers can require
//! validation before counting a page's rows.
//!
//! The width band is intentionally lenient. Variable-width columns
//! (`Y`, `V`, `C`, `A`) contribute an upper-bound allowance per
//! column rather than an exact width; fixed-width columns (`N`, `F`,
//! `I`, `D`, `T`) contribute their declared width. A page passes
//! validation when its median row body length is within `[low, high]`.

use std::collections::BTreeMap;

use opensqlany::{ApModel, Page, PageStore, PageType, SlottedPage};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};
use crate::syscolumn::{SysColumn, iter_syscolumns};
use crate::sysobject::bridge_owners_to_tables;
use crate::systable::iter_systable_entries;

/// Per-column variable-width allowance (bytes) added to the upper bound
/// when the column's domain is variable-length (`Y`, `V`, `C`, `A`).
pub const VARIABLE_COLUMN_UPPER_ALLOWANCE: u32 = 64;

/// Minimum row body length any plausible row must clear, regardless of
/// schema. SA17 rows include a small row header that is not modelled
/// per-column.
pub const MIN_ROW_BODY_BYTES: u32 = 4;

/// A row-width band derived from a table's SYSCOLUMN schema.
#[derive(Debug, Clone, Copy)]
pub struct WidthBand {
    /// SYSCOLUMN owner identifier (bridged to the table via the
    /// `SYSOBJECT` catalog; see [`crate::sysobject`]).
    pub owner_object_id: u32,
    /// Lower bound on plausible row body length.
    pub low: u32,
    /// Upper bound on plausible row body length.
    pub high: u32,
    /// Number of columns counted in the band.
    pub column_count: u32,
}

/// Schema-aware width-validator built from SYSCOLUMN + SYSTABLE.
#[derive(Debug, Clone)]
pub struct SchemaAttribution {
    /// Map from table name to width band.
    bands: BTreeMap<String, WidthBand>,
}

/// Summary of [`SchemaAttribution::validate_corpus`] over a page set.
#[derive(Debug, Default, Clone, Copy)]
pub struct ValidationStats {
    /// Pages with a position attribution that have a width band and pass it.
    pub pass: u64,
    /// Pages with a position attribution that have a width band but fail it.
    pub fail: u64,
    /// Pages with a position attribution whose table has no width band.
    pub no_band: u64,
    /// Pages whose row body could not be measured (no slot directory or no rows).
    pub unmeasured: u64,
}

impl ValidationStats {
    /// Total pages observed.
    pub fn total(&self) -> u64 {
        self.pass + self.fail + self.no_band + self.unmeasured
    }
}

/// Domain characters known to be fixed-width in SA17. All other
/// alphabetic domain characters are treated as variable-width.
fn is_fixed_width_domain(d: u8) -> bool {
    matches!(d, b'N' | b'F' | b'I' | b'D' | b'T' | b'B')
}

impl SchemaAttribution {
    /// Build the width-band index by scanning SYSTABLE + SYSCOLUMN.
    pub fn build(store: &PageStore, model: &ApModel) -> Self {
        let entries = iter_systable_entries(store, model).collect::<Vec<_>>();
        let columns: Vec<SysColumn> = iter_syscolumns(store, model).collect();

        // Bridge SYSCOLUMN.owner_object_id -> SYSTABLE.name via the
        // SYSOBJECT catalog (Phase 6, WP-6Z.2). The prior
        // assumption that owner_object_id == data_root_page was wrong;
        // it matched coincidentally on a small subset.
        let owner_to_table = bridge_owners_to_tables(store, model, &columns, &entries);

        // Group columns by owner.
        let mut cols_by_owner: BTreeMap<u32, Vec<SysColumn>> = BTreeMap::new();
        for c in columns {
            cols_by_owner.entry(c.owner_object_id).or_default().push(c);
        }
        // Index SYSTABLE entries by name.
        let mut tables_by_name: BTreeMap<String, &crate::SysTableEntry> = BTreeMap::new();
        for e in &entries {
            tables_by_name.entry(e.name.clone()).or_insert(e);
        }

        let mut bands = BTreeMap::new();
        for (owner, cols) in &cols_by_owner {
            let Some(table_name) = owner_to_table.get(owner) else {
                continue;
            };
            let Some(table) = tables_by_name.get(table_name) else {
                continue;
            };
            if cols.is_empty() {
                continue;
            }
            // Confidence gate: if SYSTABLE declares N columns but we
            // parsed fewer than half of them via SYSCOLUMN, skip rather
            // than emit a degenerate band no real page can satisfy.
            if let Some(declared) = table.col_count {
                if declared >= 2 && (cols.len() as u32) * 2 < declared as u32 {
                    continue;
                }
            }
            let mut low: u32 = MIN_ROW_BODY_BYTES;
            let mut high: u32 = MIN_ROW_BODY_BYTES;
            for c in cols {
                let w = c.width as u32;
                if is_fixed_width_domain(c.domain_char) {
                    low = low.saturating_add(w);
                    high = high.saturating_add(w);
                } else {
                    low = low.saturating_add(1);
                    high = high.saturating_add(w.max(VARIABLE_COLUMN_UPPER_ALLOWANCE));
                }
            }
            bands.insert(
                table_name.clone(),
                WidthBand {
                    owner_object_id: *owner,
                    low,
                    high,
                    column_count: cols.len() as u32,
                },
            );
        }
        Self { bands }
    }

    /// Look up the width band for `table_name`.
    pub fn band(&self, table_name: &str) -> Option<&WidthBand> {
        self.bands.get(table_name)
    }

    /// Number of tables with computed bands.
    pub fn len(&self) -> usize {
        self.bands.len()
    }

    /// True when no bands were computed (catalog unparseable).
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// Return `true` if `observed_row_body` falls within `table_name`'s
    /// width band. Returns `false` when the table has no band.
    pub fn validate_observed(&self, table_name: &str, observed_row_body: u32) -> bool {
        let Some(b) = self.bands.get(table_name) else {
            return false;
        };
        observed_row_body >= b.low && observed_row_body <= b.high
    }

    /// Compute the median live row body length for `page_number`. Returns
    /// `None` when the page cannot be decoded, has no slot directory, or
    /// has no live rows.
    pub fn measure_page(
        &self,
        store: &PageStore,
        model: &ApModel,
        page_number: u64,
    ) -> Option<u32> {
        median_row_body(store, model, page_number)
    }

    /// Validate that a page already attributed to `table_name` plausibly
    /// belongs to that table.
    pub fn validate(
        &self,
        store: &PageStore,
        model: &ApModel,
        page_number: u64,
        table_name: &str,
    ) -> Option<bool> {
        let observed = median_row_body(store, model, page_number)?;
        Some(self.validate_observed(table_name, observed))
    }

    /// Validate every entry in `(page_number, table_name)` and return
    /// aggregate statistics.
    pub fn validate_corpus<I, S>(
        &self,
        store: &PageStore,
        model: &ApModel,
        pages: I,
    ) -> ValidationStats
    where
        I: IntoIterator<Item = (u64, S)>,
        S: AsRef<str>,
    {
        let mut s = ValidationStats::default();
        for (pn, name) in pages {
            let name = name.as_ref();
            if !self.bands.contains_key(name) {
                s.no_band += 1;
                continue;
            }
            match median_row_body(store, model, pn) {
                None => s.unmeasured += 1,
                Some(m) => {
                    if self.validate_observed(name, m) {
                        s.pass += 1;
                    } else {
                        s.fail += 1;
                    }
                }
            }
        }
        s
    }
}

/// Decode `page_number` and return the median row body length across
/// live slots. Returns `None` when the page cannot be decoded, has no
/// slot directory, or has no live rows.
fn median_row_body(store: &PageStore, model: &ApModel, page_number: u64) -> Option<u32> {
    let page = store.page(page_number).ok()?;
    if page.trailer().page_type() != PageType::Extent {
        return None;
    }
    let raw = page.bytes();
    let plain = if let Some(bv) = recover_bv_qb_data(page_number, raw) {
        deobfuscate_with_bv(raw, page_number, bv)
    } else {
        model.deobfuscate_with_store(raw, page_number, store)
    };
    let p = Page::from_bytes(page_number, &plain);
    let sp = SlottedPage::parse(p);
    sp.directory.as_ref()?;
    let rows = sp.row_bytes();
    if rows.is_empty() {
        return None;
    }
    let mut lens: Vec<u32> = rows.iter().map(|(_, b)| b.len() as u32).collect();
    lens.sort_unstable();
    Some(lens[lens.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_domains_recognized() {
        for d in *b"NFIDTB" {
            assert!(is_fixed_width_domain(d));
        }
        for d in *b"YVCAX" {
            assert!(!is_fixed_width_domain(d));
        }
    }

    #[test]
    fn empty_attribution_validates_nothing() {
        let sa = SchemaAttribution {
            bands: BTreeMap::new(),
        };
        assert!(sa.is_empty());
        assert!(!sa.validate_observed("anything", 32));
    }

    #[test]
    fn observed_inside_band_passes() {
        let mut bands = BTreeMap::new();
        bands.insert(
            "t".to_string(),
            WidthBand {
                owner_object_id: 1,
                low: 16,
                high: 64,
                column_count: 3,
            },
        );
        let sa = SchemaAttribution { bands };
        assert!(sa.validate_observed("t", 32));
        assert!(!sa.validate_observed("t", 8));
        assert!(!sa.validate_observed("t", 200));
        assert!(sa.validate_observed("t", 16));
        assert!(sa.validate_observed("t", 64));
    }

    #[test]
    fn validation_stats_total_sums_components() {
        let s = ValidationStats {
            pass: 4,
            fail: 1,
            no_band: 2,
            unmeasured: 3,
        };
        assert_eq!(s.total(), 10);
    }

    #[test]
    fn unknown_table_does_not_validate() {
        let sa = SchemaAttribution {
            bands: BTreeMap::new(),
        };
        assert!(!sa.validate_observed("missing", 10));
    }
}
