//! Content-signature-based page-to-table attribution.
//!
//! Where [`crate::page_attribution::PageAttribution`] attributes pages by
//! their position relative to each `SYSTABLE` entry's `data_root_page`,
//! this module attributes pages by their **row layout**: the first row's
//! leading bytes form a column-type signature that is empirically unique
//! per table.
//!
//! Build a [`ContentAttribution`] from the corpus once via
//! [`ContentAttribution::build`], then call [`ContentAttribution::attribute`]
//! per page. The lookup is `O(log N)` over deduplicated signatures.
//!
//! The signature length is [`SIG_LEN`] = 12 bytes (the first 12 bytes of
//! `row[0]`). Per WP-4A reconnaissance (`probe_rowsig.rs`) this length
//! is sufficient to disambiguate every user table on the four-file Phase
//! 5 corpus; longer signatures add no resolving power and reduce coverage
//! on short rows.
//!
//! ## Use as a verifier
//!
//! ContentAttribution is intentionally **diagnostic-only** in this
//! release: the production line-item exporter still uses
//! `PageAttribution`. To check the existing attribution against content
//! signatures call [`ContentAttribution::compare`] which returns the
//! number of pages whose position-attribution and content-attribution
//! agree, disagree, or where one side returns `None`.

use std::collections::BTreeMap;

use opensqlany::{ApModel, Page, PageStore, PageType, SlottedPage};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};
use crate::page_attribution::PageAttribution;
use crate::systable::{SysTableEntry, iter_systable_entries};

/// Length in bytes of the row-0 prefix used as a table signature.
pub const SIG_LEN: usize = 12;

/// A row-0 prefix used as a per-table fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowSignature(Vec<u8>);

impl RowSignature {
    /// Extract a signature from `row` (truncated or padded with the row
    /// bytes available; never longer than [`SIG_LEN`]).
    pub fn from_row(row: &[u8]) -> Self {
        let take = row.len().min(SIG_LEN);
        Self(row[..take].to_vec())
    }

    /// Raw signature bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Hex representation of the signature (space-separated bytes).
    pub fn to_hex(&self) -> String {
        self.0
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Content-signature attribution map.
#[derive(Debug, Clone)]
pub struct ContentAttribution {
    /// Signature -> the SysTable entry whose `data_root_page` carries
    /// this row-0 prefix. Ambiguous signatures (shared by multiple
    /// tables) are NOT inserted: such signatures resolve to `None` to
    /// keep attribution strict.
    unique: BTreeMap<RowSignature, SysTableEntry>,
    /// Count of distinct signatures that were ambiguous (i.e. shared by
    /// two or more tables). Useful for diagnostics.
    ambiguous_count: usize,
    /// Count of `SYSTABLE` entries that contributed no usable signature
    /// (root not an E-page, no slot directory, empty rows).
    skipped_count: usize,
}

/// Statistics from comparing position-based and content-based
/// attribution over a set of pages.
#[derive(Debug, Default, Clone, Copy)]
pub struct AttributionAgreement {
    /// Pages where both attributors return the same table name.
    pub agree: u64,
    /// Pages where both return a name but the names differ.
    pub disagree: u64,
    /// Pages where only the position attributor returns a name.
    pub only_position: u64,
    /// Pages where only the content attributor returns a name.
    pub only_content: u64,
    /// Pages where neither attributor returns a name.
    pub neither: u64,
}

impl AttributionAgreement {
    /// Total pages compared.
    pub fn total(&self) -> u64 {
        self.agree + self.disagree + self.only_position + self.only_content + self.neither
    }
}

impl ContentAttribution {
    /// Build the signature map by scanning every `SYSTABLE` entry's
    /// `data_root_page` and extracting the row-0 prefix.
    pub fn build(store: &PageStore, model: &ApModel) -> Self {
        let entries: Vec<SysTableEntry> = iter_systable_entries(store, model).collect();
        let mut sig_to_tables: BTreeMap<RowSignature, Vec<SysTableEntry>> = BTreeMap::new();
        let mut skipped = 0usize;

        for entry in entries {
            let Some(sig) = entry
                .data_root_page
                .and_then(|root| extract_signature(store, model, root as u64))
            else {
                skipped += 1;
                continue;
            };
            sig_to_tables.entry(sig).or_default().push(entry);
        }

        let mut unique = BTreeMap::new();
        let mut ambiguous = 0usize;
        for (sig, mut tables) in sig_to_tables {
            if tables.len() == 1 {
                unique.insert(sig, tables.pop().expect("len == 1"));
            } else {
                ambiguous += 1;
            }
        }

        Self {
            unique,
            ambiguous_count: ambiguous,
            skipped_count: skipped,
        }
    }

    /// Look up the table whose `data_root_page` row-0 signature matches
    /// `page_number`'s row-0 signature.
    ///
    /// Returns `None` when the page cannot be decoded, has no slot
    /// directory, has no rows, or its signature is shared by multiple
    /// tables (ambiguous).
    pub fn attribute(
        &self,
        store: &PageStore,
        model: &ApModel,
        page_number: u64,
    ) -> Option<&SysTableEntry> {
        let sig = extract_signature(store, model, page_number)?;
        self.unique.get(&sig)
    }

    /// Number of unique signature -> table mappings.
    pub fn len(&self) -> usize {
        self.unique.len()
    }

    /// True when no signatures were collected.
    pub fn is_empty(&self) -> bool {
        self.unique.is_empty()
    }

    /// Number of signatures that collided across two or more tables
    /// (and were therefore excluded from the strict map).
    pub fn ambiguous_count(&self) -> usize {
        self.ambiguous_count
    }

    /// Number of SYSTABLE entries that contributed no signature.
    pub fn skipped_count(&self) -> usize {
        self.skipped_count
    }

    /// Compare the position-based attribution to this content-based
    /// attribution across `pages`. Returns the agreement breakdown.
    pub fn compare<I>(
        &self,
        store: &PageStore,
        model: &ApModel,
        position: &PageAttribution,
        pages: I,
    ) -> AttributionAgreement
    where
        I: IntoIterator<Item = u64>,
    {
        let mut out = AttributionAgreement::default();
        for pn in pages {
            let pos_name = position.attribute(pn).map(|e| e.name.as_str());
            let con_name = self.attribute(store, model, pn).map(|e| e.name.as_str());
            match (pos_name, con_name) {
                (Some(a), Some(b)) if a == b => out.agree += 1,
                (Some(_), Some(_)) => out.disagree += 1,
                (Some(_), None) => out.only_position += 1,
                (None, Some(_)) => out.only_content += 1,
                (None, None) => out.neither += 1,
            }
        }
        out
    }
}

/// Decode `page_number` and return the row-0 signature, if available.
fn extract_signature(store: &PageStore, model: &ApModel, page_number: u64) -> Option<RowSignature> {
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
    let (_slot, first_row) = rows.first()?;
    Some(RowSignature::from_row(first_row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_truncates_long_row() {
        let row = vec![1u8; 64];
        let s = RowSignature::from_row(&row);
        assert_eq!(s.as_bytes().len(), SIG_LEN);
        assert!(s.as_bytes().iter().all(|&b| b == 1));
    }

    #[test]
    fn signature_handles_short_row() {
        let row = vec![0xAB, 0xCD, 0xEF];
        let s = RowSignature::from_row(&row);
        assert_eq!(s.as_bytes(), &[0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn signature_hex_format() {
        let s = RowSignature::from_row(&[0x00, 0x0E, 0xFF]);
        assert_eq!(s.to_hex(), "00 0e ff");
    }

    #[test]
    fn agreement_total_sums_components() {
        let a = AttributionAgreement {
            agree: 5,
            disagree: 2,
            only_position: 1,
            only_content: 3,
            neither: 4,
        };
        assert_eq!(a.total(), 15);
    }
}
