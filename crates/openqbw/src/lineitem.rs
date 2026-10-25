//! Invoice line-item record parsing.
//!
//! The lineitem record is anchored by the 25-byte pattern
//! `00 00 00 00 10 <16 ASCII base62 chars> 00 00 80 3F` where the 16 chars
//! are the parent invoice's QB-ID and the trailing bytes are a `float32(1.0)`
//! quantity. Immediately after the anchor sit the typed amount bytes and a
//! date/counter pair.
//!
//! Amount-type byte semantics:
//!   - `0x02`         -- `[type][3 bytes]`, decoded per source table (see
//!     [`AmountEncoding`]): unsigned u24 on invoice lines, sign-bit u23 on
//!     general-journal lines, not decoded on any other table
//!   - `0x01`         -- meaning not established; never decoded
//!   - `0x03`         -- deferred (raw bytes recorded but not converted)
//!   - `0x00`         -- non-amount marker (record carries date/counter only)
//!   - any other byte -- reported as [`Error::UnknownAmountType`]

use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};
use crate::error::Error;
use crate::opaque::count_opaque_pages;
use crate::page_attribution::PageAttribution;
use crate::scan::{RecordFlag, ScanOptions, ScanResult, ScanWarning};

pub use crate::date::DATE_EPOCH_DAYS_BEFORE_UNIX;
use crate::date::{DateSource, sa_day_to_unix_day, scan_date_counter};

const PAGE_DATA_END: usize = 0xFF0;
const ANCHOR_LEN: usize = 25;
const QB_ID_LEN: usize = 16;
const F32_ONE: [u8; 4] = [0x00, 0x00, 0x80, 0x3F];
const PARENT_PREFIX: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x10];

/// How the 3 magnitude bytes of a `0x02` amount are interpreted for the
/// table a line item came from.
///
/// The two readings of the same bytes disagree on about half of all values,
/// so the reading is chosen per source table and recorded on every item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AmountEncoding {
    /// `[type][u24 LE]` unsigned cents. Used for invoice line-item tables.
    UnsignedU24,
    /// `[type][u23 LE magnitude + sign bit 7 of the first byte]`. Used for the
    /// general-journal line-item table, where the `verify` pair-balance check
    /// (`+x` and `-x` lines sum to zero) supports the sign bit.
    SignBitU23,
    /// The source table is unknown or has no established reading. The amount
    /// is not decoded.
    Unconfirmed,
}

impl AmountEncoding {
    /// Source table whose amounts carry a sign bit.
    pub const JOURNAL_TABLE: &'static str = "abmc_general_journal_inventory_lineitem";
    /// Source table whose amounts are unsigned.
    pub const INVOICE_TABLE: &'static str = "abmc_invoice_inventory_lineitem";

    /// Encoding for line items attributed to `source_table`.
    pub fn for_source_table(source_table: Option<&str>) -> Self {
        match source_table {
            Some(Self::JOURNAL_TABLE) => Self::SignBitU23,
            Some(Self::INVOICE_TABLE) => Self::UnsignedU24,
            _ => Self::Unconfirmed,
        }
    }

    /// Stable lowercase name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsignedU24 => "unsigned_u24",
            Self::SignBitU23 => "sign_bit_u23",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// Classification of the amount-type byte in a line item. The raw byte is
/// [`LineItem::amount_type_byte`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AmountType {
    /// `0x00` - placeholder. The four bytes are not a cents value.
    None,
    /// `0x01` - meaning not established. The amount is never decoded.
    Unestablished,
    /// `0x02` - `[0x02][3 amount bytes]`.
    Standard,
    /// `0x03` - deferred. Encoding not yet known; only the raw bytes are kept.
    Deferred,
}

impl AmountType {
    /// Classify the leading amount-type byte. `None` for an unknown byte,
    /// which scanning reports as [`Error::UnknownAmountType`].
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(Self::None),
            0x01 => Some(Self::Unestablished),
            0x02 => Some(Self::Standard),
            0x03 => Some(Self::Deferred),
            _ => None,
        }
    }
}

/// Decode `[type][3 bytes]` under `encoding`. Only type `0x02` on a table
/// with an established encoding yields a value.
fn decode_amount(ty: AmountType, encoding: AmountEncoding, raw: &[u8; 4]) -> Option<i64> {
    if ty != AmountType::Standard {
        return None;
    }
    let hi = (raw[2] as i64) | ((raw[3] as i64) << 8);
    match encoding {
        AmountEncoding::UnsignedU24 => Some((raw[1] as i64) | (hi << 8)),
        AmountEncoding::SignBitU23 => {
            let mag = ((raw[1] & 0x7F) as i64) | (hi << 8);
            Some(if raw[1] & 0x80 != 0 { -mag } else { mag })
        }
        _ => None,
    }
}

/// Parsed invoice line-item record.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LineItem {
    /// Parent invoice QB-ID (16 base62 characters).
    pub invoice_id: String,
    /// Source page number in the QBW file.
    pub page_number: u64,
    /// Byte offset within the decoded page body where the anchor starts.
    pub page_offset: usize,
    /// Item QB-ID - the line item's own identifier, when discovered.
    pub item_qb_id: Option<String>,
    /// Amount-type classification.
    pub amount_type: AmountType,
    /// Reading applied to the amount bytes, chosen from `source_table`.
    pub amount_encoding: AmountEncoding,
    /// Amount in cents under `amount_encoding`. `None` when the amount type
    /// carries no cents value (`0x00`, `0x03`), when its meaning is not
    /// established (`0x01`), or when `amount_encoding` is
    /// [`AmountEncoding::Unconfirmed`]. Callers must treat `None` as
    /// "unknown", not zero.
    pub amount_cents: Option<i64>,
    /// Raw 4 bytes immediately following the float32(1.0) quantity.
    pub amount_raw: [u8; 4],
    /// SA17 transaction date (SA-day), set only when `date_source` is
    /// [`DateSource::WindowScan`].
    pub txn_date_raw: Option<u32>,
    /// SA17 transaction counter, set under the same condition.
    pub counter: Option<u32>,
    /// How `txn_date_raw` was found. `NotFound` and `Ambiguous` leave the
    /// date and counter unset.
    pub date_source: DateSource,
    /// Owning `SYSTABLE` name (e.g. `abmc_invoice_inventory_lineitem`) when
    /// the line item's page was attributable. Populated by
    /// [`iter_lineitems_with_attribution`]; `None` otherwise.
    pub source_table: Option<String>,
    /// Reasons this record deserves less trust. Empty when no check flagged it.
    pub flags: Vec<RecordFlag>,
}

impl LineItem {
    /// The raw amount-type byte that followed the anchor.
    pub fn amount_type_byte(&self) -> u8 {
        self.amount_raw[0]
    }

    /// Build a record with the given invoice id, source table and decoded
    /// amount, all other fields empty. For tests and tools that need to
    /// synthesize records; scans never call it.
    #[doc(hidden)]
    pub fn synthetic(
        invoice_id: &str,
        source_table: Option<&str>,
        amount_cents: Option<i64>,
    ) -> Self {
        Self {
            invoice_id: invoice_id.to_owned(),
            page_number: 0,
            page_offset: 0,
            item_qb_id: None,
            amount_type: AmountType::Standard,
            amount_encoding: AmountEncoding::for_source_table(source_table),
            amount_cents,
            amount_raw: [0; 4],
            txn_date_raw: None,
            counter: None,
            date_source: DateSource::NotFound,
            source_table: source_table.map(str::to_owned),
            flags: Vec::new(),
        }
    }

    /// Attribute the item to `source_table` and decode its amount with the
    /// reading that fits that table.
    fn set_source_table(&mut self, source_table: Option<String>) {
        self.amount_encoding = AmountEncoding::for_source_table(source_table.as_deref());
        self.source_table = source_table;
        self.amount_cents = decode_amount(self.amount_type, self.amount_encoding, &self.amount_raw);
        self.flags.retain(|f| {
            !matches!(
                f,
                RecordFlag::AmountEncodingUnconfirmed | RecordFlag::AmountTypeUnestablished
            )
        });
        if self.amount_type == AmountType::Standard
            && self.amount_encoding == AmountEncoding::Unconfirmed
        {
            self.flags.push(RecordFlag::AmountEncodingUnconfirmed);
        }
        if self.amount_type == AmountType::Unestablished {
            self.flags.push(RecordFlag::AmountTypeUnestablished);
        }
    }

    /// Convert the SA17 transaction date to days since the Unix epoch
    /// (1970-01-01) with the crate's epoch offset (see [`crate::date`]).
    /// Returns `None` if the raw date is missing or outside the plausible
    /// SA-day range.
    pub fn txn_date_days_since_unix(&self) -> Option<i64> {
        self.txn_date_raw.and_then(sa_day_to_unix_day)
    }
}

/// Yields every line item found in `store` by scanning each `E`-type page.
///
/// A page that cannot be read, an anchor with an unknown amount-type byte
/// and an anchor too close to the page end each yield an `Err`; scanning
/// continues with the next record. Each yielded item has
/// `source_table = None`, so its amount is not decoded
/// ([`AmountEncoding::Unconfirmed`]).
pub fn iter_lineitems<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = Result<LineItem, Error>> + 'a {
    LineItemIter::new(store, model, None)
}

/// Like [`iter_lineitems`], but tags each emitted item with the
/// `SysTableEntry::name` of the table that the line item's source page
/// is attributed to (via [`PageAttribution`]) and decodes its amount with
/// the reading for that table. Errors are yielded as in [`iter_lineitems`].
pub fn iter_lineitems_with_attribution<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: &'a PageAttribution,
) -> impl Iterator<Item = Result<LineItem, Error>> + 'a {
    LineItemIter::new(store, model, Some(attribution))
}

/// Collect every line item in `store`.
///
/// In strict mode (the default options) the first error is returned. In
/// lenient mode each error becomes a [`ScanWarning::RecordSkipped`] in the
/// result and scanning continues; callers must report the warnings.
pub fn scan_lineitems(
    store: &PageStore,
    model: &ApModel,
    attribution: Option<&PageAttribution>,
    options: &ScanOptions,
) -> Result<ScanResult<LineItem>, Error> {
    let mut scan = ScanResult::default();
    let opaque = count_opaque_pages(store)?;
    if opaque.mostly_opaque() {
        return Err(Error::ContentOpaque {
            opaque: opaque.opaque,
            extent: opaque.extent,
        });
    }
    if opaque.opaque > 0 {
        scan.warnings.push(ScanWarning::OpaquePages {
            opaque: opaque.opaque,
            extent: opaque.extent,
        });
    }
    for r in LineItemIter::new(store, model, attribution) {
        match r {
            Ok(li) => scan.items.push(li),
            Err(e) if options.is_lenient() => scan.warnings.push(ScanWarning::RecordSkipped(e)),
            Err(e) => return Err(e),
        }
    }
    Ok(scan)
}

struct LineItemIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: Option<&'a PageAttribution>,
    pn: u64,
    n_pages: u64,
    buffer: Vec<Result<LineItem, Error>>,
}

impl<'a> LineItemIter<'a> {
    fn new(
        store: &'a PageStore,
        model: &'a ApModel,
        attribution: Option<&'a PageAttribution>,
    ) -> Self {
        Self {
            store,
            model,
            attribution,
            pn: 1, // skip superblock (page 0)
            n_pages: store.page_count(),
            buffer: Vec::new(),
        }
    }

    fn fill_buffer(&mut self) -> Result<bool, Error> {
        while self.buffer.is_empty() && self.pn < self.n_pages {
            let pn = self.pn;
            self.pn += 1;

            let page = self
                .store
                .page(pn)
                .map_err(|e| Error::from_store(e, Some(pn)))?;
            if page.trailer().page_type() != PageType::Extent {
                continue;
            }

            let raw = page.bytes();
            // Prefer QB-specific anchor-based bv recovery (C.36). Falls back
            // to the generic AP model when the anchor is not present.
            let plain = match recover_bv_qb_data(pn, raw) {
                Some(bv) => deobfuscate_with_bv(raw, pn, bv),
                None => self.model.deobfuscate_with_store(raw, pn, self.store),
            };
            let before = self.buffer.len();
            scan_page(&plain[..PAGE_DATA_END], pn, &mut self.buffer);
            if let Some(attr) = self.attribution
                && let Some(entry) = attr.attribute(pn)
            {
                let name = entry.name.clone();
                for li in self.buffer[before..].iter_mut().flatten() {
                    li.set_source_table(Some(name.clone()));
                }
            }
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for LineItemIter<'_> {
    type Item = Result<LineItem, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.buffer.pop() {
                return Some(item);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                Ok(false) => return None,
                // A page read error is yielded; the page counter already
                // moved past it, so the next call continues with the next page.
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl FusedIterator for LineItemIter<'_> {}

/// Scan a decoded page body for line-item anchors and append parsed records
/// to `out`. Items are appended in the order encountered.
fn scan_page(body: &[u8], pn: u64, out: &mut Vec<Result<LineItem, Error>>) {
    if body.len() < ANCHOR_LEN {
        return;
    }
    let limit = body.len() - ANCHOR_LEN;
    let mut start = Vec::new();
    let mut pos = 0usize;
    while pos <= limit {
        // Cheap pre-filter: look for the 5-byte parent prefix.
        if body[pos..pos + 5] != PARENT_PREFIX {
            pos += 1;
            continue;
        }
        // Verify QB-ID chars are base62.
        let id_range = pos + 5..pos + 5 + QB_ID_LEN;
        if !is_base62(&body[id_range.clone()]) {
            pos += 1;
            continue;
        }
        // Verify trailing float32(1.0).
        if body[pos + 5 + QB_ID_LEN..pos + ANCHOR_LEN] != F32_ONE {
            pos += 1;
            continue;
        }
        start.push(pos);
        pos += ANCHOR_LEN;
    }

    // Reverse so we can pop in original order (iterator buffer is LIFO).
    // Each record's search windows stop at its neighbours' anchors, so a
    // field of the previous or next record is never read as this one's.
    let bounds: Vec<(usize, usize, usize)> = (0..start.len())
        .map(|i| {
            let floor = if i == 0 {
                0
            } else {
                start[i - 1] + ANCHOR_LEN + 4
            };
            let ceil = start.get(i + 1).copied().unwrap_or(body.len());
            (start[i], floor, ceil)
        })
        .collect();
    for (anchor, floor, ceil) in bounds.into_iter().rev() {
        out.push(parse_anchor(body, pn, anchor, floor, ceil));
    }
}

fn parse_anchor(
    body: &[u8],
    pn: u64,
    anchor_start: usize,
    floor: usize,
    ceil: usize,
) -> Result<LineItem, Error> {
    let id_start = anchor_start + 5;
    let invoice_id = std::str::from_utf8(&body[id_start..id_start + QB_ID_LEN])
        .expect("anchor guarded by is_base62")
        .to_owned();

    let payload_start = anchor_start + ANCHOR_LEN;
    if payload_start + 4 > body.len() {
        return Err(Error::TruncatedAmount {
            page_number: pn,
            page_offset: anchor_start,
        });
    }
    let mut amount_raw = [0u8; 4];
    amount_raw.copy_from_slice(&body[payload_start..payload_start + 4]);
    let Some(amount_type) = AmountType::from_byte(amount_raw[0]) else {
        return Err(Error::UnknownAmountType {
            page_number: pn,
            page_offset: anchor_start,
            type_byte: amount_raw[0],
        });
    };

    // Forward search for a date+counter pair after the amount bytes.
    let scan = scan_date_counter(
        body,
        payload_start + 4,
        (payload_start + 64).min(ceil.saturating_sub(7)),
        1_000_000,
    );

    // Backward search for the nearest item QB-ID prefixed by `04 00 10`,
    // not reaching into the previous record.
    let back_start = anchor_start.saturating_sub(96).max(floor);
    let item_qb_id = find_item_id_nearest(&body[back_start..anchor_start]);

    let mut li = LineItem {
        invoice_id,
        page_number: pn,
        page_offset: anchor_start,
        item_qb_id,
        amount_type,
        amount_encoding: AmountEncoding::Unconfirmed,
        amount_cents: None,
        amount_raw,
        txn_date_raw: scan.date,
        counter: scan.counter,
        date_source: scan.source,
        source_table: None,
        flags: Vec::new(),
    };
    li.set_source_table(None);
    if li.date_source == DateSource::Ambiguous {
        li.flags.push(RecordFlag::DateAmbiguous);
    }
    Ok(li)
}

fn is_base62(s: &[u8]) -> bool {
    s.iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

/// The `04 00 10 <16 base62>` item ID closest to the end of `window`.
fn find_item_id_nearest(window: &[u8]) -> Option<String> {
    const PREFIX: [u8; 3] = [0x04, 0x00, 0x10];
    if window.len() < PREFIX.len() + QB_ID_LEN {
        return None;
    }
    (0..=window.len() - PREFIX.len() - QB_ID_LEN)
        .rev()
        .find_map(|rel| {
            let id = &window[rel + 3..rel + 3 + QB_ID_LEN];
            (window[rel..rel + 3] == PREFIX && is_base62(id))
                .then(|| String::from_utf8_lossy(id).into_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_type_classification() {
        assert_eq!(AmountType::from_byte(0x00), Some(AmountType::None));
        assert_eq!(AmountType::from_byte(0x01), Some(AmountType::Unestablished));
        assert_eq!(AmountType::from_byte(0x02), Some(AmountType::Standard));
        assert_eq!(AmountType::from_byte(0x03), Some(AmountType::Deferred));
        assert_eq!(AmountType::from_byte(0x42), None);
    }

    #[test]
    fn decode_known_record_both_readings() {
        // 02 40 0f 04: unsigned 0x040f40 = 266,048 ($2,660.48).
        let raw = [0x02, 0x40, 0x0F, 0x04];
        let u = decode_amount(AmountType::Standard, AmountEncoding::UnsignedU24, &raw);
        assert_eq!(u, Some(266_048));
    }

    #[test]
    fn type_03_has_no_cents() {
        let raw = [0x03, 0xBF, 0x5F, 0x63];
        for e in [AmountEncoding::UnsignedU24, AmountEncoding::SignBitU23] {
            assert_eq!(decode_amount(AmountType::Deferred, e, &raw), None);
        }
    }

    #[test]
    fn sign_bit_pairs_sum_to_zero() {
        // C.48 example: paired 2-line journal lines must sum to zero.
        let pos = [0x02, 0x40, 0x2D, 0x01];
        let neg = [0x02, 0xC0, 0x2D, 0x01];
        let e = AmountEncoding::SignBitU23;
        let a = decode_amount(AmountType::Standard, e, &pos).unwrap();
        let b = decode_amount(AmountType::Standard, e, &neg).unwrap();
        assert_eq!((a, b), (77_120, -77_120));
    }

    #[test]
    fn sign_bit_magnitude_reaches_23_bits() {
        let raw = [0x02, 0x7F, 0xFF, 0xFF];
        let v = decode_amount(AmountType::Standard, AmountEncoding::SignBitU23, &raw);
        assert_eq!(v, Some(16_777_087));
    }

    #[test]
    fn base62_check() {
        assert!(is_base62(b"0000000000001QBm"));
        assert!(!is_base62(b"0000000000001QB!"));
        assert!(!is_base62(b"0000000000001 QB"));
    }

    /// Body of `len` bytes with one anchor at `at`, followed by as much of
    /// `payload` as fits.
    fn anchor_body(len: usize, at: usize, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![0xAAu8; len];
        body[at..at + 5].copy_from_slice(&PARENT_PREFIX);
        body[at + 5..at + 5 + QB_ID_LEN].copy_from_slice(b"0000000000001QBm");
        body[at + 5 + QB_ID_LEN..at + ANCHOR_LEN].copy_from_slice(&F32_ONE);
        let end = (at + ANCHOR_LEN + payload.len()).min(len);
        body[at + ANCHOR_LEN..end].copy_from_slice(&payload[..end - at - ANCHOR_LEN]);
        body
    }

    fn scan_one(body: &[u8]) -> Result<LineItem, Error> {
        let mut out = Vec::new();
        scan_page(body, 7, &mut out);
        assert_eq!(out.len(), 1);
        out.pop().unwrap()
    }

    #[test]
    fn scan_page_finds_synthetic_anchor() {
        let mut body = anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x0F, 0x04]);
        // date u32 LE 15511, counter u32 LE 12671 at offset payload+8
        let p = 32 + ANCHOR_LEN;
        body[p + 8..p + 12].copy_from_slice(&15511u32.to_le_bytes());
        body[p + 12..p + 16].copy_from_slice(&12671u32.to_le_bytes());

        let li = scan_one(&body).unwrap();
        assert_eq!(li.invoice_id, "0000000000001QBm");
        assert_eq!(li.amount_type, AmountType::Standard);
        assert_eq!(li.amount_type_byte(), 0x02);
        assert_eq!(li.amount_raw, [0x02, 0x40, 0x0F, 0x04]);
        assert_eq!(li.txn_date_raw, Some(15511));
        assert_eq!(li.counter, Some(12671));
        assert_eq!(li.date_source, DateSource::WindowScan);
        // No source table: the reading is not established, so no amount.
        assert_eq!(li.amount_encoding, AmountEncoding::Unconfirmed);
        assert_eq!(li.amount_cents, None);
    }

    #[test]
    fn journal_line_decodes_with_sign_bit() {
        // C.48 pair: 02 40 2d 01 = +771.20 and 02 c0 2d 01 = -771.20.
        // The unsigned reading of the second would be +772.48.
        let mut pos = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x2D, 0x01])).unwrap();
        let mut neg = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0xC0, 0x2D, 0x01])).unwrap();
        pos.set_source_table(Some(AmountEncoding::JOURNAL_TABLE.to_owned()));
        neg.set_source_table(Some(AmountEncoding::JOURNAL_TABLE.to_owned()));
        assert_eq!(pos.amount_encoding, AmountEncoding::SignBitU23);
        assert_eq!(pos.amount_cents, Some(77_120));
        assert_eq!(neg.amount_cents, Some(-77_120));
    }

    #[test]
    fn invoice_line_decodes_unsigned() {
        // 02 80 0f 04 is $2,661.12 on an invoice line; the sign-bit reading
        // would give -$2,659.84.
        let mut li = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0x80, 0x0F, 0x04])).unwrap();
        li.set_source_table(Some(AmountEncoding::INVOICE_TABLE.to_owned()));
        assert_eq!(li.amount_encoding, AmountEncoding::UnsignedU24);
        assert_eq!(li.amount_cents, Some(266_112));
    }

    #[test]
    fn unlisted_table_leaves_amount_unknown() {
        let mut li = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0x80, 0x0F, 0x04])).unwrap();
        li.set_source_table(Some("abmc_credit_memo_inventory_lineitem".to_owned()));
        assert_eq!(li.amount_encoding, AmountEncoding::Unconfirmed);
        assert_eq!(li.amount_cents, None);
    }

    #[test]
    fn types_without_cents_stay_unknown_on_known_tables() {
        for first in [0x00u8, 0x01, 0x03] {
            let mut li =
                scan_one(&anchor_body(PAGE_DATA_END, 32, &[first, 0x80, 0x0F, 0x04])).unwrap();
            li.set_source_table(Some(AmountEncoding::INVOICE_TABLE.to_owned()));
            assert_eq!(li.amount_cents, None, "type {first:#x}");
        }
    }

    #[test]
    fn date_after_2013_is_found() {
        let mut body = anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x0F, 0x04]);
        let p = 32 + ANCHOR_LEN;
        // SA-day 23817 is unix day 19800 (2024) under the crate's offset.
        body[p + 8..p + 12].copy_from_slice(&23_817u32.to_le_bytes());
        body[p + 12..p + 16].copy_from_slice(&500u32.to_le_bytes());
        let li = scan_one(&body).unwrap();
        assert_eq!(li.txn_date_raw, Some(23_817));
        assert_eq!(li.txn_date_days_since_unix(), Some(19_800));
    }

    #[test]
    fn two_date_pairs_leave_the_date_unset() {
        let mut body = anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x0F, 0x04]);
        let p = 32 + ANCHOR_LEN;
        body[p + 8..p + 12].copy_from_slice(&23_817u32.to_le_bytes());
        body[p + 12..p + 16].copy_from_slice(&500u32.to_le_bytes());
        body[p + 24..p + 28].copy_from_slice(&15_000u32.to_le_bytes());
        body[p + 28..p + 32].copy_from_slice(&7u32.to_le_bytes());
        let li = scan_one(&body).unwrap();
        assert_eq!(li.date_source, DateSource::Ambiguous);
        assert!(li.flags.contains(&RecordFlag::DateAmbiguous));
        assert_eq!(li.txn_date_raw, None);
    }

    #[test]
    fn close_records_keep_their_own_item_ids() {
        // Two records 60 bytes apart; the second record's window would
        // include the first record's item id (which sits earlier).
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        let put = |body: &mut Vec<u8>, at: usize, item: &[u8; 16]| {
            body[at - 19..at - 16].copy_from_slice(&[0x04, 0x00, 0x10]);
            body[at - 16..at].copy_from_slice(item);
            body[at..at + 5].copy_from_slice(&PARENT_PREFIX);
            body[at + 5..at + 21].copy_from_slice(b"0000000000001QBm");
            body[at + 21..at + 25].copy_from_slice(&F32_ONE);
            body[at + 25..at + 29].copy_from_slice(&[0x02, 0x40, 0x0F, 0x04]);
        };
        put(&mut body, 100, b"AAAAAAAAAAAAAAAA");
        put(&mut body, 160, b"BBBBBBBBBBBBBBBB");
        let mut out = Vec::new();
        scan_page(&body, 7, &mut out);
        let ids: Vec<_> = out
            .into_iter()
            .map(|r| r.unwrap().item_qb_id.unwrap())
            .collect();
        // Output is LIFO (last record first).
        assert_eq!(ids, ["BBBBBBBBBBBBBBBB", "AAAAAAAAAAAAAAAA"]);
    }

    #[test]
    fn item_id_belonging_to_previous_record_is_not_taken() {
        // Second record has no item id of its own.
        let mut body = anchor_body(PAGE_DATA_END, 100, &[0x02, 0x40, 0x0F, 0x04]);
        body[81..84].copy_from_slice(&[0x04, 0x00, 0x10]);
        body[84..100].copy_from_slice(b"AAAAAAAAAAAAAAAA");
        let second = 140;
        body[second..second + 5].copy_from_slice(&PARENT_PREFIX);
        body[second + 5..second + 21].copy_from_slice(b"0000000000001QBm");
        body[second + 21..second + 25].copy_from_slice(&F32_ONE);
        body[second + 25..second + 29].copy_from_slice(&[0x02, 0x40, 0x0F, 0x04]);
        let mut out = Vec::new();
        scan_page(&body, 7, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].as_ref().unwrap().item_qb_id, None);
        assert_eq!(
            out[1].as_ref().unwrap().item_qb_id.as_deref(),
            Some("AAAAAAAAAAAAAAAA")
        );
    }

    #[test]
    fn unknown_amount_type_is_an_error() {
        let err = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x5A, 0x01, 0x02, 0x03])).unwrap_err();
        assert!(matches!(
            err,
            Error::UnknownAmountType {
                page_number: 7,
                page_offset: 32,
                type_byte: 0x5A
            }
        ));
    }

    #[test]
    fn anchor_near_tail_reads_its_amount() {
        // Anchor ends at 0xFDE; the amount straddles the old 0xFE0 cut-off.
        let li = scan_one(&anchor_body(
            PAGE_DATA_END,
            0xFC5,
            &[0x02, 0x40, 0x0F, 0x04],
        ))
        .unwrap();
        assert_eq!(li.amount_type, AmountType::Standard);
        assert_eq!(li.amount_type_byte(), 0x02);
        assert_eq!(li.amount_raw, [0x02, 0x40, 0x0F, 0x04]);
    }

    #[test]
    fn anchor_with_cut_off_amount_is_an_error() {
        let err = scan_one(&anchor_body(
            PAGE_DATA_END,
            PAGE_DATA_END - ANCHOR_LEN - 2,
            &[0x02, 0x40],
        ))
        .unwrap_err();
        assert!(matches!(err, Error::TruncatedAmount { .. }));
    }

    fn random_extent_store(pages: usize, opaque: usize) -> PageStore {
        let mut b = vec![0u8; 4096 * pages];
        let mut x: u32 = 0x1234_5678;
        for (i, page) in b.chunks_mut(4096).enumerate().skip(1) {
            if i <= opaque {
                for byte in page[..0xFF0].iter_mut() {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    *byte = (x >> 16) as u8;
                }
            }
            page[0xFF2] = b'E';
        }
        PageStore::from_bytes(b).unwrap()
    }

    #[test]
    fn mostly_opaque_file_is_an_error() {
        let store = random_extent_store(11, 8);
        let model = ApModel::learn(&store);
        let err = scan_lineitems(&store, &model, None, &ScanOptions::strict()).unwrap_err();
        assert!(matches!(
            err,
            Error::ContentOpaque {
                opaque: 8,
                extent: 10
            }
        ));
    }

    #[test]
    fn a_few_opaque_pages_are_a_warning_even_in_strict_mode() {
        let store = random_extent_store(11, 2);
        let model = ApModel::learn(&store);
        let scan = scan_lineitems(&store, &model, None, &ScanOptions::strict()).unwrap();
        assert!(matches!(
            scan.warnings.as_slice(),
            [ScanWarning::OpaquePages {
                opaque: 2,
                extent: 10
            }]
        ));
    }
}
