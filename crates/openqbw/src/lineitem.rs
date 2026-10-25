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
//!   - any other byte -- reported as [`LineItemError::UnknownAmountType`]

use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};
use crate::page_attribution::PageAttribution;

pub use crate::date::DATE_EPOCH_DAYS_BEFORE_UNIX;

const PAGE_DATA_END: usize = 0xFF0;
const ANCHOR_LEN: usize = 25;
const QB_ID_LEN: usize = 16;
const F32_ONE: [u8; 4] = [0x00, 0x00, 0x80, 0x3F];
const PARENT_PREFIX: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x10];

/// Errors from line-item parsing.
#[derive(Debug, thiserror::Error)]
pub enum LineItemError {
    /// Wrapped `opensqlany` error.
    #[error(transparent)]
    Sa(#[from] opensqlany::Error),
    /// A page could not be read from the page store.
    #[error("page {page_number}: {source}")]
    Page {
        /// Page that failed.
        page_number: u64,
        /// Underlying page-store error.
        source: opensqlany::Error,
    },
    /// A line-item anchor was followed by an amount-type byte that is not
    /// one of the known codes `0x00..=0x03`. The anchor may be a false
    /// positive, or the file may use an encoding this crate does not know.
    #[error(
        "page {page_number} offset {page_offset:#x}: unknown amount-type byte {type_byte:#04x} after line-item anchor"
    )]
    UnknownAmountType {
        /// Page holding the anchor.
        page_number: u64,
        /// Offset of the anchor within the decoded page.
        page_offset: usize,
        /// The unrecognised type byte.
        type_byte: u8,
    },
    /// A line-item anchor ends too close to the end of the page data area
    /// for its 4 amount bytes to be read.
    #[error(
        "page {page_number} offset {page_offset:#x}: line-item anchor leaves fewer than 4 bytes for the amount"
    )]
    TruncatedAmount {
        /// Page holding the anchor.
        page_number: u64,
        /// Offset of the anchor within the decoded page.
        page_offset: usize,
    },
}

/// How the 3 magnitude bytes of a `0x02` amount are interpreted for the
/// table a line item came from.
///
/// The two readings of the same bytes disagree on about half of all values,
/// so the reading is chosen per source table and recorded on every item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Classification of the amount-type byte in a line item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmountType {
    /// `0x00` - placeholder. The four bytes are not a cents value.
    None,
    /// `0x01` - meaning not established. The amount is never decoded.
    OneByteOne,
    /// `0x02` - `[0x02][u24 LE cents]`.
    Standard,
    /// `0x03` - deferred. Encoding not yet known; only `raw` is populated.
    Deferred,
    /// Any other byte. Scanning reports it as
    /// [`LineItemError::UnknownAmountType`]; no `LineItem` carries it.
    Other(u8),
}

impl AmountType {
    /// Classify the leading amount-type byte.
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::None,
            0x01 => Self::OneByteOne,
            0x02 => Self::Standard,
            0x03 => Self::Deferred,
            other => Self::Other(other),
        }
    }

    /// Read the 4 amount bytes as UNSIGNED integer cents (the raw u24 LE in
    /// `raw[1..4]`). A raw reading only: [`LineItem::amount_cents_decoded`] applies
    /// the reading that fits the item's source table.
    pub fn decode_cents(self, raw: &[u8; 4]) -> Option<u32> {
        match self {
            Self::Standard | Self::OneByteOne => {
                let cents = (raw[1] as u32) | ((raw[2] as u32) << 8) | ((raw[3] as u32) << 16);
                Some(cents)
            }
            _ => None,
        }
    }

    /// Read the 4 amount bytes as SIGNED integer cents under the
    /// high-bit-of-byte-1-is-sign convention (C.48 Track A). The magnitude
    /// is `(raw[1] & 0x7F) | (raw[2] << 8) | (raw[3] << 16)` and the sign
    /// is `-1` if `raw[1] & 0x80 != 0`. Returns `None` for types other than
    /// `0x01` and `0x02` (type `0x03` is NOT a signed amount; its bytes
    /// store enumerated reference codes - see C.48).
    pub fn decode_cents_signed(self, raw: &[u8; 4]) -> Option<i32> {
        match self {
            Self::Standard | Self::OneByteOne => {
                let mag =
                    ((raw[1] & 0x7F) as i32) | ((raw[2] as i32) << 8) | ((raw[3] as i32) << 16);
                let sign = if raw[1] & 0x80 != 0 { -1 } else { 1 };
                Some(sign * mag)
            }
            _ => None,
        }
    }
}

/// Parsed invoice line-item record.
#[derive(Debug, Clone)]
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
    /// Undecoded UNSIGNED u24 reading of the amount bytes (type `0x01`/`0x02`).
    /// Wrong for sign-bit amounts. Use [`LineItem::amount_cents_decoded`].
    #[deprecated(note = "unsigned reading of the raw bytes; use amount_cents_decoded")]
    pub amount_cents: Option<u32>,
    /// Undecoded SIGNED (bit 7 of the first byte) reading of the amount
    /// bytes. Wrong for unsigned amounts. Use [`LineItem::amount_cents_decoded`].
    #[deprecated(note = "sign-bit reading of the raw bytes; use amount_cents_decoded")]
    pub amount_cents_signed: Option<i32>,
    /// Amount in cents under `amount_encoding`. `None` when the amount type
    /// carries no cents value (`0x00`, `0x03`), when its meaning is not
    /// established (`0x01`), or when `amount_encoding` is
    /// [`AmountEncoding::Unconfirmed`]. Callers must treat `None` as
    /// "unknown", not zero.
    pub amount_cents_decoded: Option<i64>,
    /// Raw 4 bytes immediately following the float32(1.0) quantity.
    pub amount_raw: [u8; 4],
    /// SA17 transaction date stored as days since 1981-01-01, when discovered.
    pub txn_date_raw: Option<u32>,
    /// SA17 transaction counter, when discovered.
    pub counter: Option<u32>,
    /// Owning `SYSTABLE` name (e.g. `abmc_invoice_inventory_lineitem`) when
    /// the line item's page was attributable. Populated by
    /// [`iter_lineitems_with_attribution`]; `None` otherwise.
    pub source_table: Option<String>,
}

impl LineItem {
    /// Attribute the item to `source_table` and decode its amount with the
    /// reading that fits that table.
    fn set_source_table(&mut self, source_table: Option<String>) {
        self.amount_encoding = AmountEncoding::for_source_table(source_table.as_deref());
        self.source_table = source_table;
        self.amount_cents_decoded = match (self.amount_type, self.amount_encoding) {
            (AmountType::Standard, AmountEncoding::UnsignedU24) => self
                .amount_type
                .decode_cents(&self.amount_raw)
                .map(i64::from),
            (AmountType::Standard, AmountEncoding::SignBitU23) => self
                .amount_type
                .decode_cents_signed(&self.amount_raw)
                .map(i64::from),
            _ => None,
        };
    }

    /// Convert the SA17 transaction date to days since the Unix epoch (1970-01-01).
    /// Returns `None` if the raw date is missing or implausible (over ~30k days
    /// past the SA epoch).
    pub fn txn_date_days_since_unix(&self) -> Option<i64> {
        self.txn_date_raw
            .filter(|&d| d > 0 && d < 30_000)
            .map(|d| d as i64 - DATE_EPOCH_DAYS_BEFORE_UNIX)
    }
}

/// Yields every line item found in `store` by scanning each `E`-type page.
///
/// A page that cannot be read, an anchor with an unknown amount-type byte
/// and an anchor too close to the page end each yield an `Err`; scanning
/// continues with the next record. Each yielded item has
/// `source_table = None`, so its amount is not decoded
/// ([`AmountEncoding::Unconfirmed`]).
pub fn iter_lineitems_checked<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = Result<LineItem, LineItemError>> + 'a {
    LineItemIter::new(store, model, None)
}

/// Like [`iter_lineitems_checked`], but tags each emitted item with the
/// `SysTableEntry::name` of the table that the line item's source page
/// is attributed to (via [`PageAttribution`]) and decodes its amount with
/// the reading for that table. Errors are yielded as in [`iter_lineitems_checked`].
pub fn iter_lineitems_checked_with_attribution<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: &'a PageAttribution,
) -> impl Iterator<Item = Result<LineItem, LineItemError>> + 'a {
    LineItemIter::new(store, model, Some(attribution))
}

/// Yields line items without surfacing record errors.
///
/// Anchors with an unknown amount-type byte are yielded as items with
/// [`AmountType::Other`]; an anchor too close to the page end is yielded with
/// zeroed amount bytes; a page read error ends the iteration. Prefer
/// [`iter_lineitems_checked`] or [`scan_lineitems`].
#[deprecated(note = "hides undecodable records; use iter_lineitems_checked or scan_lineitems")]
pub fn iter_lineitems<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = LineItem> + 'a {
    let mut it = LineItemIter::new(store, model, None);
    it.legacy = true;
    it.map_while(Result::ok)
}

/// Like [`iter_lineitems`], with source-table attribution. Same caveats.
#[deprecated(
    note = "hides undecodable records; use iter_lineitems_checked_with_attribution or scan_lineitems"
)]
pub fn iter_lineitems_with_attribution<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: &'a PageAttribution,
) -> impl Iterator<Item = LineItem> + 'a {
    let mut it = LineItemIter::new(store, model, Some(attribution));
    it.legacy = true;
    it.map_while(Result::ok)
}

/// Result of [`scan_lineitems`].
#[derive(Debug, Default)]
pub struct LineItemScan {
    /// Line items found, in page order.
    pub items: Vec<LineItem>,
    /// Records skipped in lenient mode, each with the reason. Always empty
    /// in strict mode.
    pub rejected: Vec<LineItemError>,
}

/// Collect every line item in `store`.
///
/// In strict mode (`lenient == false`) the first error is returned. In
/// lenient mode each error is moved to [`LineItemScan::rejected`] and
/// scanning continues; callers must report that list.
pub fn scan_lineitems(
    store: &PageStore,
    model: &ApModel,
    attribution: Option<&PageAttribution>,
    lenient: bool,
) -> Result<LineItemScan, LineItemError> {
    let mut scan = LineItemScan::default();
    for r in LineItemIter::new(store, model, attribution) {
        match r {
            Ok(li) => scan.items.push(li),
            Err(e) if lenient => scan.rejected.push(e),
            Err(e) => return Err(e),
        }
    }
    Ok(scan)
}

struct LineItemIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: Option<&'a PageAttribution>,
    legacy: bool,
    pn: u64,
    n_pages: u64,
    buffer: Vec<Result<LineItem, LineItemError>>,
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
            legacy: false,
            pn: 1, // skip superblock (page 0)
            n_pages: store.page_count(),
            buffer: Vec::new(),
        }
    }

    fn fill_buffer(&mut self) -> Result<bool, LineItemError> {
        while self.buffer.is_empty() && self.pn < self.n_pages {
            let pn = self.pn;
            self.pn += 1;

            let page = self.store.page(pn).map_err(|source| LineItemError::Page {
                page_number: pn,
                source,
            })?;
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
            scan_page(&plain[..PAGE_DATA_END], pn, self.legacy, &mut self.buffer);
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
    type Item = Result<LineItem, LineItemError>;

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
fn scan_page(body: &[u8], pn: u64, legacy: bool, out: &mut Vec<Result<LineItem, LineItemError>>) {
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
    for anchor in start.into_iter().rev() {
        out.push(parse_anchor(body, pn, anchor, legacy));
    }
}

fn parse_anchor(
    body: &[u8],
    pn: u64,
    anchor_start: usize,
    legacy: bool,
) -> Result<LineItem, LineItemError> {
    let id_start = anchor_start + 5;
    let invoice_id = std::str::from_utf8(&body[id_start..id_start + QB_ID_LEN])
        .expect("anchor guarded by is_base62")
        .to_owned();

    let payload_start = anchor_start + ANCHOR_LEN;
    let mut amount_raw = [0u8; 4];
    if payload_start + 4 <= body.len() {
        amount_raw.copy_from_slice(&body[payload_start..payload_start + 4]);
    } else if !legacy {
        return Err(LineItemError::TruncatedAmount {
            page_number: pn,
            page_offset: anchor_start,
        });
    }
    let amount_type = AmountType::from_byte(amount_raw[0]);
    if let (AmountType::Other(type_byte), false) = (amount_type, legacy) {
        return Err(LineItemError::UnknownAmountType {
            page_number: pn,
            page_offset: anchor_start,
            type_byte,
        });
    }

    // Forward search for date+counter pair: SA date as u32 LE in 13000..20000,
    // followed by 4-byte counter > 0.
    let mut txn_date_raw = None;
    let mut counter = None;
    let end = (payload_start + 64).min(body.len().saturating_sub(8));
    for off in (payload_start + 4)..end {
        let d = u32::from_le_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        let c = u32::from_le_bytes([body[off + 4], body[off + 5], body[off + 6], body[off + 7]]);
        if (13_000..20_000).contains(&d) && c > 0 && c < 1_000_000 {
            txn_date_raw = Some(d);
            counter = Some(c);
            break;
        }
    }

    // Backward search for item QB-ID prefixed by `04 00 10`.
    let mut item_qb_id = None;
    let back_start = anchor_start.saturating_sub(96);
    let back_slice = &body[back_start..anchor_start];
    if let Some(rel) = find_subslice(back_slice, &[0x04, 0x00, 0x10]) {
        let id_off = rel + 3;
        if id_off + QB_ID_LEN <= back_slice.len() {
            let id_bytes = &back_slice[id_off..id_off + QB_ID_LEN];
            if is_base62(id_bytes) {
                item_qb_id = Some(std::str::from_utf8(id_bytes).unwrap().to_owned());
            }
        }
    }

    #[allow(deprecated)]
    let mut li = LineItem {
        invoice_id,
        page_number: pn,
        page_offset: anchor_start,
        item_qb_id,
        amount_type,
        amount_encoding: AmountEncoding::Unconfirmed,
        amount_cents: amount_type.decode_cents(&amount_raw),
        amount_cents_signed: amount_type.decode_cents_signed(&amount_raw),
        amount_cents_decoded: None,
        amount_raw,
        txn_date_raw,
        counter,
        source_table: None,
    };
    li.set_source_table(None);
    Ok(li)
}

fn is_base62(s: &[u8]) -> bool {
    s.iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    for i in 0..=haystack.len() - needle.len() {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_type_classification() {
        assert_eq!(AmountType::from_byte(0x00), AmountType::None);
        assert_eq!(AmountType::from_byte(0x01), AmountType::OneByteOne);
        assert_eq!(AmountType::from_byte(0x02), AmountType::Standard);
        assert_eq!(AmountType::from_byte(0x03), AmountType::Deferred);
        assert_eq!(AmountType::from_byte(0x42), AmountType::Other(0x42));
    }

    #[test]
    fn cents_decoding_known_record() {
        // Violette line item 1: 02 40 0f 04 → cents 0x040f40 = 266,048 → $2,660.48
        let raw = [0x02, 0x40, 0x0F, 0x04];
        let t = AmountType::from_byte(raw[0]);
        assert_eq!(t.decode_cents(&raw), Some(266_048));
    }

    #[test]
    fn type_03_no_cents() {
        let raw = [0x03, 0xBF, 0x5F, 0x63];
        let t = AmountType::from_byte(raw[0]);
        assert_eq!(t.decode_cents(&raw), None);
        assert_eq!(t.decode_cents_signed(&raw), None);
    }

    #[test]
    fn signed_decode_positive() {
        // C.48 example: 0x02 with high bit clear -> positive.
        // raw = 02 40 2d 01 -> magnitude (0x40 & 0x7F)|(0x2D<<8)|(0x01<<16) = 0x012D40 = 77120
        let raw = [0x02, 0x40, 0x2D, 0x01];
        let t = AmountType::from_byte(raw[0]);
        assert_eq!(t.decode_cents_signed(&raw), Some(77_120));
    }

    #[test]
    fn signed_decode_negative_pairs_with_positive() {
        // C.48 example: paired 2-line journal lines must sum to zero.
        // line1: 02 40 2d 01 -> +77120
        // line2: 02 c0 2d 01 -> -77120
        let pos = [0x02, 0x40, 0x2D, 0x01];
        let neg = [0x02, 0xC0, 0x2D, 0x01];
        let t = AmountType::from_byte(pos[0]);
        let a = t.decode_cents_signed(&pos).unwrap();
        let b = t.decode_cents_signed(&neg).unwrap();
        assert_eq!(a + b, 0);
        assert_eq!(a, 77_120);
        assert_eq!(b, -77_120);
    }

    #[test]
    fn signed_decode_large_magnitude() {
        // Verify magnitude reaches full 23-bit range.
        // 02 7F FF FF -> +(0x7F | 0xFF00 | 0xFF0000) = 0xFFFF7F = 16_777_087
        let raw = [0x02, 0x7F, 0xFF, 0xFF];
        let t = AmountType::from_byte(raw[0]);
        assert_eq!(t.decode_cents_signed(&raw), Some(16_777_087));
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

    fn scan_one(body: &[u8]) -> Result<LineItem, LineItemError> {
        let mut out = Vec::new();
        scan_page(body, 7, false, &mut out);
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
        assert_eq!(li.amount_raw, [0x02, 0x40, 0x0F, 0x04]);
        assert_eq!(li.txn_date_raw, Some(15511));
        assert_eq!(li.counter, Some(12671));
        // No source table: the reading is not established, so no amount.
        assert_eq!(li.amount_encoding, AmountEncoding::Unconfirmed);
        assert_eq!(li.amount_cents_decoded, None);
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
        assert_eq!(pos.amount_cents_decoded, Some(77_120));
        assert_eq!(neg.amount_cents_decoded, Some(-77_120));
    }

    #[test]
    fn invoice_line_decodes_unsigned() {
        // 02 80 0f 04 is $2,661.12 on an invoice line; the sign-bit reading
        // would give -$2,659.84.
        let mut li = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0x80, 0x0F, 0x04])).unwrap();
        li.set_source_table(Some(AmountEncoding::INVOICE_TABLE.to_owned()));
        assert_eq!(li.amount_encoding, AmountEncoding::UnsignedU24);
        assert_eq!(li.amount_cents_decoded, Some(266_112));
    }

    #[test]
    fn unlisted_table_leaves_amount_unknown() {
        let mut li = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0x80, 0x0F, 0x04])).unwrap();
        li.set_source_table(Some("abmc_credit_memo_inventory_lineitem".to_owned()));
        assert_eq!(li.amount_encoding, AmountEncoding::Unconfirmed);
        assert_eq!(li.amount_cents_decoded, None);
    }

    #[test]
    fn types_without_cents_stay_unknown_on_known_tables() {
        for first in [0x00u8, 0x01, 0x03] {
            let mut li =
                scan_one(&anchor_body(PAGE_DATA_END, 32, &[first, 0x80, 0x0F, 0x04])).unwrap();
            li.set_source_table(Some(AmountEncoding::INVOICE_TABLE.to_owned()));
            assert_eq!(li.amount_cents_decoded, None, "type {first:#x}");
        }
    }

    #[test]
    fn unknown_amount_type_is_an_error() {
        let err = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x5A, 0x01, 0x02, 0x03])).unwrap_err();
        assert!(matches!(
            err,
            LineItemError::UnknownAmountType {
                page_number: 7,
                page_offset: 32,
                type_byte: 0x5A
            }
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_mode_keeps_old_records_and_raw_readings() {
        let mut out = Vec::new();
        scan_page(
            &anchor_body(PAGE_DATA_END, 32, &[0x5A, 1, 2, 3]),
            7,
            true,
            &mut out,
        );
        let li = out.pop().unwrap().unwrap();
        assert_eq!(li.amount_type, AmountType::Other(0x5A));
        let li = scan_one_legacy(&anchor_body(PAGE_DATA_END, 32, &[0x02, 0xC0, 0x2D, 0x01]));
        assert_eq!(li.amount_cents, Some(77_248));
        assert_eq!(li.amount_cents_signed, Some(-77_120));
        assert_eq!(li.amount_cents_decoded, None);
    }

    #[allow(deprecated)]
    fn scan_one_legacy(body: &[u8]) -> LineItem {
        let mut out = Vec::new();
        scan_page(body, 7, true, &mut out);
        out.pop().unwrap().unwrap()
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
        assert!(matches!(err, LineItemError::TruncatedAmount { .. }));
    }
}
