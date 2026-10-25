//! Options, results and warnings shared by every reading entry point.
//!
//! Strict is the default everywhere: input the decoder cannot read with
//! confidence is an error. [`Leniency`] names the checks a caller may
//! relax; each relaxed problem is reported on the result, never dropped.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};

use opensqlany::{ApModel, PageStore, PageType};

use crate::decode::{DecodedPage, decode_page};
use crate::error::{Error, Result};
use crate::page_attribution::AttributionGap;

/// The set of checks a read may relax.
///
/// The default, [`Leniency::STRICT`], relaxes nothing. New flags are added
/// in minor releases; combine flags with `|` and test with
/// [`Leniency::contains`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Leniency {
    bits: u32,
}

impl Leniency {
    /// Relax nothing.
    pub const STRICT: Leniency = Leniency { bits: 0 };
    /// Skip a record or page that cannot be decoded (an unknown amount-type
    /// byte, an amount cut off by the page end, a page that fails to
    /// decode) and list it in [`Scan::rejected`] instead of returning the
    /// error.
    pub const RECORDS: Leniency = Leniency { bits: 1 };

    /// `true` when no check is relaxed.
    pub const fn is_strict(self) -> bool {
        self.bits == 0
    }

    /// `true` when every flag in `other` is also set in `self`.
    pub const fn contains(self, other: Leniency) -> bool {
        self.bits & other.bits == other.bits
    }

    /// The union of two sets.
    pub const fn union(self, other: Leniency) -> Leniency {
        Leniency {
            bits: self.bits | other.bits,
        }
    }
}

impl BitOr for Leniency {
    type Output = Leniency;

    fn bitor(self, rhs: Leniency) -> Leniency {
        self.union(rhs)
    }
}

impl BitOrAssign for Leniency {
    fn bitor_assign(&mut self, rhs: Leniency) {
        *self = self.union(rhs);
    }
}

/// Options for opening and reading a file. `ReadOptions::default()` is
/// strict. New fields are added in minor releases; build with
/// [`ReadOptions::strict`], [`ReadOptions::lenient`] or
/// [`ReadOptions::with_leniency`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadOptions {
    /// Which checks may be relaxed.
    pub leniency: Leniency,
}

impl ReadOptions {
    /// Strict options (the default).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Options that relax exactly [`Leniency::RECORDS`]. A flag added in a
    /// later release is not added to this set.
    pub fn lenient() -> Self {
        Self::default().with_leniency(Leniency::RECORDS)
    }

    /// These options with `leniency` in place of the current set.
    pub fn with_leniency(mut self, leniency: Leniency) -> Self {
        self.leniency = leniency;
        self
    }

    /// `true` when undecodable records are skipped and listed.
    pub(crate) fn skips_records(&self) -> bool {
        self.leniency.contains(Leniency::RECORDS)
    }
}

/// A reason a record that was kept deserves less trust. Records carry a
/// `Vec<RecordFlag>`; an empty list means no check flagged the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecordFlag {
    /// The amount's source table has no established encoding, so the amount
    /// was not decoded.
    AmountEncodingUnconfirmed,
    /// The amount type is one whose meaning is not established, so the
    /// amount was not decoded.
    AmountTypeUnestablished,
    /// Two or more separate date/counter pairs sat in the search window, so
    /// no date was chosen.
    DateAmbiguous,
    /// The row held more than one 16-character ID in the header ID shape, or
    /// row boundaries were unknown, so the ID taken may be a foreign key.
    HeaderIdAmbiguous,
    /// No check confirmed the AP keystream of the record's page; the page
    /// was decoded with the best-scoring key, which may be wrong.
    KeystreamUnverified,
    /// Some bytes the record was read from lie in a 512-byte sector whose
    /// keystream step could not be told apart from its runner-up.
    LowConfidenceSector,
    /// A `SYSTABLE` row's `data_root_page` is past the end of the file or
    /// not a data (`E`) page, so it cannot be the table's root. The row is
    /// not used for page-to-table attribution.
    RootPageInvalid,
}

impl RecordFlag {
    /// Stable snake_case name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AmountEncodingUnconfirmed => "amount_encoding_unconfirmed",
            Self::AmountTypeUnestablished => "amount_type_unestablished",
            Self::DateAmbiguous => "date_ambiguous",
            Self::HeaderIdAmbiguous => "header_id_ambiguous",
            Self::KeystreamUnverified => "keystream_unverified",
            Self::LowConfidenceSector => "low_confidence_sector",
            Self::RootPageInvalid => "root_page_invalid",
        }
    }
}

impl fmt::Display for RecordFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of a run-level [`Warning`].
///
/// New kinds are added in minor releases; match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WarningKind {
    /// Some `E` pages are indistinguishable from random bytes and yield no
    /// records (compressed or separately encrypted blobs).
    #[non_exhaustive]
    OpaquePages {
        /// Opaque `E` pages.
        opaque: u64,
        /// All `E` pages.
        extent: u64,
    },
    /// Pages decoded with a keystream no check confirmed. Records read from
    /// them carry [`RecordFlag::KeystreamUnverified`].
    #[non_exhaustive]
    KeystreamUnverifiedPages {
        /// Pages affected.
        pages: u64,
        /// Pages decoded.
        decoded: u64,
    },
    /// Pages with at least one low-confidence sector. Records touching such
    /// a sector carry [`RecordFlag::LowConfidenceSector`].
    #[non_exhaustive]
    LowConfidenceSectorPages {
        /// Pages affected.
        pages: u64,
        /// Pages decoded.
        decoded: u64,
    },
    /// Pages whose slot directory the page prelude did not confirm, so row
    /// boundaries were not used.
    #[non_exhaustive]
    UnconfirmedDirectoryPages {
        /// Pages affected.
        pages: u64,
    },
    /// Record anchors found in a page's free space, between the slot
    /// directory and the lowest live row, were left out: they are residue of
    /// deleted or moved records.
    #[non_exhaustive]
    RecordsOutsideLiveRows {
        /// Anchors left out.
        records: u64,
    },
    /// Live rows on header pages that hold no 16-character header ID and so
    /// gave no transaction header.
    #[non_exhaustive]
    HeaderRowsWithoutId {
        /// Rows affected.
        rows: u64,
    },
    /// Records whose page is not attributed to any table. Their
    /// `source_table` is `None` and line-item amounts are not decoded.
    #[non_exhaustive]
    UnattributedRecords {
        /// Records without a source table.
        records: u64,
        /// All records in the scan.
        total: u64,
    },
    /// Line-item anchors whose quantity field is a plausible value other than
    /// 1.0. The decoder reads only lines with quantity 1.0, so these lines are
    /// missing from the results and from every total.
    #[non_exhaustive]
    LineItemsNotRead {
        /// Anchors seen.
        anchors: u64,
    },
    /// `SYSTABLE` rows whose `data_root_page` cannot be a root (past the end
    /// of the file or not a data page). They carry
    /// [`RecordFlag::RootPageInvalid`] and are not used for attribution.
    #[non_exhaustive]
    InvalidRootPages {
        /// Rows affected.
        tables: u64,
    },
    /// Page-to-table attribution is unavailable, so records have no
    /// source table and header scans find nothing.
    AttributionGap(AttributionGap),
    /// Any other condition, named by a stable short code.
    Other(&'static str),
}

impl WarningKind {
    /// Stable snake_case code.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpaquePages { .. } => "opaque_pages",
            Self::KeystreamUnverifiedPages { .. } => "keystream_unverified_pages",
            Self::LowConfidenceSectorPages { .. } => "low_confidence_sector_pages",
            Self::UnconfirmedDirectoryPages { .. } => "unconfirmed_directory_pages",
            Self::RecordsOutsideLiveRows { .. } => "records_outside_live_rows",
            Self::HeaderRowsWithoutId { .. } => "header_rows_without_id",
            Self::UnattributedRecords { .. } => "unattributed_records",
            Self::LineItemsNotRead { .. } => "line_items_not_read",
            Self::InvalidRootPages { .. } => "invalid_root_pages",
            Self::AttributionGap(_) => "attribution_gap",
            Self::Other(code) => code,
        }
    }
}

/// A condition that applies to a whole read rather than to one record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Warning {
    /// What was found.
    pub kind: WarningKind,
}

impl Warning {
    /// A warning of `kind`.
    pub fn new(kind: WarningKind) -> Self {
        Self { kind }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WarningKind::OpaquePages { opaque, extent } => write!(
                f,
                "{opaque} of {extent} data pages are opaque and were not decoded"
            ),
            WarningKind::KeystreamUnverifiedPages { pages, decoded } => write!(
                f,
                "{pages} of {decoded} data pages were decoded with an unconfirmed keystream; \
                 their records carry keystream_unverified"
            ),
            WarningKind::LowConfidenceSectorPages { pages, decoded } => write!(
                f,
                "{pages} of {decoded} data pages have a low-confidence sector; records touching \
                 one carry low_confidence_sector"
            ),
            WarningKind::UnconfirmedDirectoryPages { pages } => write!(
                f,
                "{pages} pages have a slot directory the page prelude does not confirm"
            ),
            WarningKind::RecordsOutsideLiveRows { records } => write!(
                f,
                "{records} record anchors in page free space (deleted or moved records) were \
                 left out"
            ),
            WarningKind::HeaderRowsWithoutId { rows } => write!(
                f,
                "{rows} rows on header pages hold no header ID and gave no transaction"
            ),
            WarningKind::UnattributedRecords { records, total } => write!(
                f,
                "{records} of {total} records are on pages not attributed to any table; they \
                 have no source table and line-item amounts are not decoded"
            ),
            WarningKind::LineItemsNotRead { anchors } => write!(
                f,
                "{anchors} line-item anchors have a quantity other than 1.0; this version reads \
                 only quantity-1.0 lines, so these are missing from the results and totals"
            ),
            WarningKind::InvalidRootPages { tables } => write!(
                f,
                "{tables} SYSTABLE rows name a data_root_page that is past the end of the file \
                 or not a data page; they are not used for attribution"
            ),
            WarningKind::AttributionGap(AttributionGap::NoCatalogRows) => write!(
                f,
                "no SYSTABLE rows found; page-to-table attribution is unavailable"
            ),
            WarningKind::AttributionGap(AttributionGap::AllRootsZeroed) => write!(
                f,
                "every SYSTABLE data_root_page is zero, so page-to-table attribution is \
                 unavailable (seen on some QuickBooks Enterprise 24.0 files, openqbw#16)"
            ),
            WarningKind::Other(code) => f.write_str(code),
        }
    }
}

/// Result of a scan: the records found, the records a lenient read skipped,
/// and run-level warnings.
#[derive(Debug)]
#[non_exhaustive]
pub struct Scan<T> {
    /// Records found, in page order.
    pub items: Vec<T>,
    /// Records or pages skipped under [`Leniency::RECORDS`], each with the
    /// error that would have been returned. Always empty in strict mode.
    pub rejected: Vec<Error>,
    /// Conditions that apply to the whole read, in both modes.
    pub warnings: Vec<Warning>,
}

impl<T> Default for Scan<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            rejected: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl<T> Scan<T> {
    /// Return `e` in strict mode; list it in `rejected` when records may be
    /// skipped.
    pub(crate) fn reject(&mut self, e: Error, options: &ReadOptions) -> Result<()> {
        if options.skips_records() {
            self.rejected.push(e);
            Ok(())
        } else {
            Err(e)
        }
    }

    /// The items with another element type, keeping rejections and warnings.
    pub(crate) fn map_items<U>(self, f: impl FnOnce(Vec<T>) -> Vec<U>) -> Scan<U> {
        Scan {
            items: f(self.items),
            rejected: self.rejected,
            warnings: self.warnings,
        }
    }
}

/// Decode every `E` page for which `want` returns `true` and hand it to
/// `f`. A page that fails to decode is rejected per `options`. Pages decoded
/// with less than full confidence are counted into `scan.warnings`.
pub(crate) fn for_each_extent_page<T>(
    store: &PageStore,
    model: &ApModel,
    options: &ReadOptions,
    scan: &mut Scan<T>,
    mut want: impl FnMut(u64) -> bool,
    mut f: impl FnMut(&DecodedPage, &mut Scan<T>) -> Result<()>,
) -> Result<()> {
    let (mut decoded, mut unverified, mut weak) = (0u64, 0u64, 0u64);
    // Page 0 is the plaintext superblock.
    for pn in 1..store.page_count() {
        if !want(pn) {
            continue;
        }
        let page = store.page(pn).map_err(|e| Error::from_store(e, Some(pn)))?;
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        let page = match decode_page(store, model, pn) {
            Ok(p) => p,
            Err(e) => {
                scan.reject(e, options)?;
                continue;
            }
        };
        decoded += 1;
        unverified += page.keystream_unverified() as u64;
        weak += page.has_weak_sectors() as u64;
        f(&page, scan)?;
    }
    if unverified > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::KeystreamUnverifiedPages {
                pages: unverified,
                decoded,
            }));
    }
    if weak > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::LowConfidenceSectorPages {
                pages: weak,
                decoded,
            }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_strict() {
        assert!(ReadOptions::default().leniency.is_strict());
        assert!(!ReadOptions::default().skips_records());
        assert!(ReadOptions::lenient().skips_records());
    }

    #[test]
    fn strict_scan_returns_the_rejection() {
        let mut scan: Scan<()> = Scan::default();
        let e = Error::TruncatedAmount {
            page_number: 1,
            page_offset: 2,
        };
        assert!(scan.reject(e, &ReadOptions::strict()).is_err());
        assert!(scan.rejected.is_empty());
        let e = Error::TruncatedAmount {
            page_number: 1,
            page_offset: 2,
        };
        scan.reject(e, &ReadOptions::lenient()).unwrap();
        assert_eq!(scan.rejected.len(), 1);
    }
}
