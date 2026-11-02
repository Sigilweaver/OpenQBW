//! The entry point: [`Reader`] opens a file and reads its records.

use std::path::Path;

use opensqlany::{ApModel, PageStore, PageType};

use crate::error::{Error, Result};
use crate::lineitem::{LineItem, scan_lineitems};
use crate::open::{learn_model, open_store, store_from_bytes};
use crate::page_attribution::{AttributionGap, PageAttribution};
use crate::scan::{ReadOptions, RecordFlag, Scan, Warning, WarningKind};
use crate::syscolumn::{SchemaRecovery, SysColumn};
use crate::sysindex::SysIndexEntry;
use crate::systable::SysTableEntry;
use crate::transaction_header::{TransactionHeader, scan_transaction_headers};

/// An open QuickBooks company file.
///
/// Opening checks the superblock magic, version and flags and the CRC and
/// trailer of every page, calibrates the page keystream and reads the
/// `SYSTABLE` catalog, which page-to-table attribution is built from. The
/// [`ReadOptions`] given to [`Reader::open`] apply to every read.
#[derive(Debug)]
#[non_exhaustive]
pub struct Reader {
    store: PageStore,
    model: ApModel,
    options: ReadOptions,
    created: u32,
    catalog: Scan<SysTableEntry>,
    attribution: PageAttribution,
}

impl Reader {
    /// Open the file at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) when the file cannot be read; a
    /// format error ([`ErrorKind`](crate::ErrorKind) `BadMagic`,
    /// `UnsupportedVersion`, `UnsupportedFlags`, `BadCrc`, `BadTrailer`)
    /// when it is not a supported QuickBooks company file or a page is
    /// damaged; `Store` when the keystream cannot be calibrated; and, in
    /// strict mode, the first catalog page that fails to decode.
    pub fn open(path: impl AsRef<Path>, options: &ReadOptions) -> Result<Self> {
        Self::from_store(open_store(path.as_ref())?, options)
    }

    /// As [`Reader::open`], over the bytes of a file already in memory.
    pub fn from_bytes(bytes: Vec<u8>, options: &ReadOptions) -> Result<Self> {
        Self::from_store(store_from_bytes(bytes)?, options)
    }

    fn from_store(store: PageStore, options: &ReadOptions) -> Result<Self> {
        let model = learn_model(&store)?;
        let created = creation_stamp(&store)?;
        let mut catalog = crate::systable::collect_unique(&store, &model, options)?;
        let invalid = flag_invalid_roots(&store, &mut catalog.items);
        if invalid > 0 {
            catalog
                .warnings
                .push(Warning::new(WarningKind::InvalidRootPages {
                    tables: invalid,
                }));
        }
        let attribution = PageAttribution::from_catalog(catalog.items.clone());
        if let Some(gap) = attribution.gap() {
            catalog
                .warnings
                .push(Warning::new(WarningKind::AttributionGap(gap)));
        }
        Ok(Self {
            store,
            model,
            options: *options,
            created,
            catalog,
            attribution,
        })
    }

    /// The options this reader was opened with.
    pub fn options(&self) -> &ReadOptions {
        &self.options
    }

    /// The database creation stamp from superblock offset 0x2A, as stored: a
    /// `u32` count of minutes since 1600-02-29.
    pub fn created_raw(&self) -> u32 {
        self.created
    }

    /// The database creation time as Unix seconds (the stored count taken as UTC), or `None` when the
    /// stamp falls outside 1990-01-01..2100-01-01.
    pub fn created_unix_seconds(&self) -> Option<i64> {
        crate::date::stamp_to_unix_seconds(self.created)
    }

    /// Number of 4096-byte pages.
    pub fn page_count(&self) -> u64 {
        self.store.page_count()
    }

    /// Size of the file in bytes.
    pub fn file_size(&self) -> u64 {
        self.store.size_bytes()
    }

    /// The deduplicated `SYSTABLE` catalog read when the file was opened,
    /// with the pages a lenient open skipped and its warnings (including a
    /// [`WarningKind::AttributionGap`]).
    pub fn catalog(&self) -> &Scan<SysTableEntry> {
        &self.catalog
    }

    /// Page-to-table attribution built from the catalog.
    pub fn attribution(&self) -> &PageAttribution {
        &self.attribution
    }

    /// Why page-to-table attribution is unavailable, if it is.
    pub fn attribution_gap(&self) -> Option<AttributionGap> {
        self.attribution.gap()
    }

    /// Every line item, each attributed to its source table by page position
    /// (unverified) and carrying its decoded numeric value.
    pub fn line_items(&self) -> Result<Scan<LineItem>> {
        let mut scan = scan_lineitems(
            &self.store,
            &self.model,
            Some(&self.attribution),
            &self.options,
        )?;
        self.note_gap(&mut scan);
        Ok(scan)
    }

    /// Every transaction header on pages attributed to a `*_header` table.
    pub fn transactions(&self) -> Result<Scan<TransactionHeader>> {
        let mut scan =
            scan_transaction_headers(&self.store, &self.model, &self.attribution, &self.options)?;
        self.note_gap(&mut scan);
        Ok(scan)
    }

    /// The deduplicated `SYSINDEX` catalog.
    pub fn indexes(&self) -> Result<Scan<SysIndexEntry>> {
        crate::sysindex::collect_unique(&self.store, &self.model, &self.options)
    }

    /// The deduplicated `SYSCOLUMN` catalog, ordered by owner and column id.
    pub fn columns(&self) -> Result<Scan<SysColumn>> {
        crate::syscolumn::collect_unique(&self.store, &self.model, &self.options)
    }

    /// Look up the columns of `table`, with what the lookup found on the way
    /// so a miss can be explained.
    pub fn schema(&self, table: &str) -> Result<SchemaRecovery> {
        crate::syscolumn::recover_schema(&self.store, &self.model, table, &self.options)
    }

    pub(crate) fn store(&self) -> &PageStore {
        &self.store
    }

    pub(crate) fn model(&self) -> &ApModel {
        &self.model
    }

    fn note_gap<T>(&self, scan: &mut Scan<T>) {
        if let Some(gap) = self.attribution.gap() {
            scan.warnings
                .push(Warning::new(WarningKind::AttributionGap(gap)));
        }
    }
}

/// The `u32` at superblock offset 0x2A.
fn creation_stamp(store: &PageStore) -> Result<u32> {
    let page = store.page(0).map_err(|e| Error::from_store(e, Some(0)))?;
    let b = page.bytes();
    Ok(u32::from_le_bytes([b[0x2A], b[0x2B], b[0x2C], b[0x2D]]))
}

/// Flag catalog rows whose `data_root_page` is past the end of the store or
/// not an `E` page (`root_page_invalid`), and rows whose `last_page` fails
/// the same test (`last_page_invalid`). Returns how many roots were flagged.
fn flag_invalid_roots(store: &PageStore, entries: &mut [SysTableEntry]) -> u64 {
    let is_extent = |p: u32| {
        store
            .page(u64::from(p))
            .is_ok_and(|p| p.trailer().page_type() == PageType::Extent)
    };
    let mut flagged = 0;
    for e in entries {
        if e.last_page.is_some_and(|p| p > 0 && !is_extent(p)) {
            e.flags.push(RecordFlag::LastPageInvalid);
        }
        let Some(root) = e.data_root_page.filter(|&p| p > 0) else {
            continue;
        };
        if !is_extent(root) {
            e.flags.push(RecordFlag::RootPageInvalid);
            flagged += 1;
        }
    }
    flagged
}
