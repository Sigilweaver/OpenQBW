//! Page-to-table attribution driven by the `SYSTABLE` catalog.
//!
//! The SA17 page-store stores each table as a B-tree, but we have not yet
//! reverse-engineered the prev/next sibling pointers between leaves
//! (see `re/NOTES.md` §C.45). Until B-tree traversal is available we
//! attribute pages to tables by a position heuristic:
//!
//! 1. Collect every `SysTableEntry` whose `data_root_page` is known and
//!    non-zero.
//! 2. Sort by `data_root_page` ascending.
//! 3. For a query page `p`, pick the entry whose `data_root_page` is the
//!    largest value still `<= p` (the "owning" table whose root is
//!    immediately below `p`).
//! 4. If that entry has a known `last_page` and `p > last_page`, the
//!    query page is considered outside any table's window: return `None`.
//!
//! The heuristic relies on the empirical observation that QB lays tables
//! down in roughly contiguous extents and writes new pages monotonically.
//! It can misclassify pages that belong to a higher-rooted table whose
//! leaves are interleaved with a lower-rooted table's overflow leaves;
//! such cases will surface as anomalous lineitem counts during
//! validation.

use std::collections::BTreeMap;

use opensqlany::{ApModel, PageStore};

use crate::systable::{SysTableEntry, collect_unique};

/// Why [`PageAttribution`] has no usable entries, when it doesn't.
///
/// Distinguishing these matters: they call for different next steps, and
/// look identical from `is_empty()` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributionGap {
    /// No `SYSTABLE` rows were found in the store at all - the catalog scan
    /// itself came up empty.
    NoCatalogRows,
    /// `SYSTABLE` rows were found, but every one has `data_root_page` absent
    /// or zero, so there is no B-tree root to anchor the position heuristic
    /// on. Observed on some QuickBooks Enterprise 24.0 files (openqbw#16):
    /// the block/extent scan `collect_unique` already uses to find rows is
    /// the only route to the data on these files, but per-page table
    /// attribution isn't available without a real B-tree walk (not yet
    /// implemented - see this module's doc comment).
    AllRootsZeroed,
}

/// Resolves page numbers to the `SYSTABLE` entry that most likely owns them.
#[derive(Debug, Clone)]
pub struct PageAttribution {
    /// Entries sorted by `data_root_page` ascending. Only entries with a
    /// non-zero `data_root_page` are retained.
    by_root: Vec<SysTableEntry>,
    gap: Option<AttributionGap>,
}

impl PageAttribution {
    /// Build an attribution map from a pre-collected, deduplicated catalog.
    pub fn from_catalog(catalog: Vec<SysTableEntry>) -> Self {
        let total = catalog.len();
        let mut by_root: Vec<SysTableEntry> = catalog
            .into_iter()
            .filter(|e| matches!(e.data_root_page, Some(p) if p > 0))
            .collect();
        let gap = if !by_root.is_empty() {
            None
        } else if total == 0 {
            Some(AttributionGap::NoCatalogRows)
        } else {
            Some(AttributionGap::AllRootsZeroed)
        };
        by_root.sort_by_key(|e| e.data_root_page.unwrap_or(0));
        Self { by_root, gap }
    }

    /// Why attribution is unavailable, if it is. `None` whenever at least
    /// one catalog entry has a usable `data_root_page` (the common case).
    #[inline]
    pub fn gap(&self) -> Option<AttributionGap> {
        self.gap
    }

    /// Convenience: read SYSTABLE from `store`/`model` and build the map.
    pub fn build(store: &PageStore, model: &ApModel) -> Self {
        Self::from_catalog(collect_unique(store, model))
    }

    /// Look up the table that most likely owns `page_number`.
    pub fn attribute(&self, page_number: u64) -> Option<&SysTableEntry> {
        if self.by_root.is_empty() {
            return None;
        }
        // Binary search for the largest data_root <= page_number.
        let key = page_number as u32;
        let idx = self
            .by_root
            .partition_point(|e| e.data_root_page.unwrap_or(0) <= key);
        if idx == 0 {
            return None;
        }
        let entry = &self.by_root[idx - 1];
        // Trim by last_page only when it is meaningful (some catalogs
        // record a last_page that is smaller than data_root_page; in
        // those cases the bound is not a true upper limit and is
        // ignored).
        if let (Some(last), Some(root)) = (entry.last_page, entry.data_root_page)
            && last >= root
            && (page_number as u32) > last
        {
            return None;
        }
        Some(entry)
    }

    /// Number of catalog entries that contribute to attribution (i.e. have
    /// a non-zero `data_root_page`).
    pub fn len(&self) -> usize {
        self.by_root.len()
    }

    /// True when no catalog entries are eligible for attribution.
    pub fn is_empty(&self) -> bool {
        self.by_root.is_empty()
    }

    /// Iterate the underlying catalog entries in `data_root_page` order.
    pub fn entries(&self) -> impl Iterator<Item = &SysTableEntry> {
        self.by_root.iter()
    }

    /// Group pages by their attributed table name. Pages with no
    /// attribution are bucketed under the empty string `""`.
    pub fn group_pages<I>(&self, pages: I) -> BTreeMap<String, Vec<u64>>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for pn in pages {
            let key = self
                .attribute(pn)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            out.entry(key).or_default().push(pn);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tid: u32, name: &str, root: Option<u32>, last: Option<u32>) -> SysTableEntry {
        SysTableEntry {
            table_id: tid,
            name: name.to_owned(),
            magic: [0; 4],
            col_count: None,
            data_root_page: root,
            last_page: last,
            page_number: 0,
            row_offset: 0,
        }
    }

    #[test]
    fn empty_catalog_returns_none() {
        let attr = PageAttribution::from_catalog(vec![]);
        assert!(attr.is_empty());
        assert!(attr.attribute(123).is_none());
        assert_eq!(attr.gap(), Some(AttributionGap::NoCatalogRows));
    }

    #[test]
    fn all_zero_roots_reports_distinct_gap() {
        // Rows were found, but none carry a usable data_root_page - the
        // situation reported against a QuickBooks Enterprise 24.0 file
        // (openqbw#16), distinct from finding no rows at all.
        let attr = PageAttribution::from_catalog(vec![
            entry(1, "a", None, None),
            entry(2, "b", Some(0), None),
        ]);
        assert!(attr.is_empty());
        assert_eq!(attr.gap(), Some(AttributionGap::AllRootsZeroed));
    }

    #[test]
    fn usable_roots_report_no_gap() {
        let attr = PageAttribution::from_catalog(vec![entry(1, "a", Some(100), Some(200))]);
        assert_eq!(attr.gap(), None);
    }

    #[test]
    fn skips_entries_without_root() {
        let attr = PageAttribution::from_catalog(vec![
            entry(1, "a", None, None),
            entry(2, "b", Some(0), None),
            entry(3, "c", Some(100), Some(200)),
        ]);
        assert_eq!(attr.len(), 1);
        assert_eq!(attr.attribute(150).unwrap().name, "c");
    }

    #[test]
    fn picks_nearest_lower_root() {
        let attr = PageAttribution::from_catalog(vec![
            entry(1, "alpha", Some(100), Some(200)),
            entry(2, "beta", Some(300), Some(400)),
            entry(3, "gamma", Some(500), Some(600)),
        ]);
        // Below first root: no attribution.
        assert!(attr.attribute(50).is_none());
        // At a root: that table owns it.
        assert_eq!(attr.attribute(100).unwrap().name, "alpha");
        // Within alpha's window.
        assert_eq!(attr.attribute(150).unwrap().name, "alpha");
        // Past alpha's last_page but below beta's root: no attribution.
        assert!(attr.attribute(250).is_none());
        // At beta's root.
        assert_eq!(attr.attribute(300).unwrap().name, "beta");
        // Within gamma's window.
        assert_eq!(attr.attribute(550).unwrap().name, "gamma");
        // Past gamma's last_page: no attribution.
        assert!(attr.attribute(700).is_none());
    }

    #[test]
    fn missing_last_page_extends_indefinitely() {
        let attr = PageAttribution::from_catalog(vec![
            entry(1, "alpha", Some(100), None),
            entry(2, "beta", Some(500), Some(600)),
        ]);
        // alpha has no last_page, so it owns everything up to beta's root.
        assert_eq!(attr.attribute(400).unwrap().name, "alpha");
        // beta still bounded by its last_page.
        assert!(attr.attribute(700).is_none());
    }

    #[test]
    fn group_pages_buckets_correctly() {
        let attr = PageAttribution::from_catalog(vec![
            entry(1, "alpha", Some(100), Some(200)),
            entry(2, "beta", Some(300), Some(400)),
        ]);
        let groups = attr.group_pages(vec![50u64, 150, 250, 350, 500]);
        assert_eq!(groups.get("alpha").unwrap(), &vec![150]);
        assert_eq!(groups.get("beta").unwrap(), &vec![350]);
        assert_eq!(groups.get("").unwrap(), &vec![50, 250, 500]);
    }
}
