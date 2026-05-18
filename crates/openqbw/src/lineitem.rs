//! Invoice line-item record parsing.
//!
//! The lineitem record is anchored by the 25-byte pattern
//! `00 00 00 00 10 <16 ASCII base62 chars> 00 00 80 3F` where the 16 chars
//! are the parent invoice's QB-ID and the trailing bytes are a `float32(1.0)`
//! quantity. Immediately after the anchor sit the typed amount bytes and a
//! date/counter pair.
//!
//! See `OpenQBW/re/NOTES.md §C.40` for the typed-amount byte semantics:
//!   * `0x01`, `0x02` — `[type][u24 LE cents]`
//!   * `0x03`         — deferred (raw bytes recorded but not converted)
//!   * `0x00`         — non-amount marker (record skipped or carries
//!                       date/counter only)

use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType, Result as SaResult};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};

/// Number of days between the Unix epoch (1970-01-01) and the SA17 epoch (1981-01-01).
/// SA stores dates as `u32` days since 1981-01-01.
pub const DATE_EPOCH_DAYS_BEFORE_UNIX: i64 = 4017;

const PAGE_DATA_END: usize = 0xFE0;
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
}

/// Classification of the amount-type byte in a line item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmountType {
    /// `0x00` — placeholder. The four bytes are not a cents value.
    None,
    /// `0x01` — `[0x01][u24 LE cents]` (provisional encoding).
    OneByteOne,
    /// `0x02` — `[0x02][u24 LE cents]`.
    Standard,
    /// `0x03` — deferred. Encoding not yet known; only `raw` is populated.
    Deferred,
    /// Any other byte — likely an anchor false-positive or unrecognised tag.
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

    /// Decode the 4 amount bytes into integer cents when the type byte is
    /// one of the known cents-bearing types (`0x01`, `0x02`).
    pub fn decode_cents(self, raw: &[u8; 4]) -> Option<u32> {
        match self {
            Self::Standard | Self::OneByteOne => {
                let cents =
                    (raw[1] as u32) | ((raw[2] as u32) << 8) | ((raw[3] as u32) << 16);
                Some(cents)
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
    /// Item QB-ID — the line item's own identifier, when discovered.
    pub item_qb_id: Option<String>,
    /// Amount-type classification.
    pub amount_type: AmountType,
    /// Decoded cents value when [`AmountType::decode_cents`] applies.
    pub amount_cents: Option<u32>,
    /// Raw 4 bytes immediately following the float32(1.0) quantity.
    pub amount_raw: [u8; 4],
    /// SA17 transaction date stored as days since 1981-01-01, when discovered.
    pub txn_date_raw: Option<u32>,
    /// SA17 transaction counter, when discovered.
    pub counter: Option<u32>,
}

impl LineItem {
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
/// Pages whose `bv` cannot be recovered are skipped silently.
pub fn iter_lineitems<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = LineItem> + 'a {
    LineItemIter::new(store, model)
}

struct LineItemIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    pn: u64,
    n_pages: u64,
    buffer: Vec<LineItem>,
}

impl<'a> LineItemIter<'a> {
    fn new(store: &'a PageStore, model: &'a ApModel) -> Self {
        Self {
            store,
            model,
            pn: 1, // skip superblock (page 0)
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
            // Prefer QB-specific anchor-based bv recovery (C.36). Falls back
            // to the generic AP model when the anchor is not present.
            let plain = match recover_bv_qb_data(pn, raw) {
                Some(bv) => deobfuscate_with_bv(raw, pn, bv),
                None => self.model.deobfuscate_with_store(raw, pn, self.store),
            };
            scan_page(&plain[..PAGE_DATA_END], pn, &mut self.buffer);
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for LineItemIter<'_> {
    type Item = LineItem;

    fn next(&mut self) -> Option<Self::Item> {
        // Errors from individual pages are swallowed: a corrupted page should
        // not abort an entire export. A future API may expose per-page errors.
        loop {
            if let Some(item) = self.buffer.pop() {
                return Some(item);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                _ => return None,
            }
        }
    }
}

impl FusedIterator for LineItemIter<'_> {}

/// Scan a decoded page body for line-item anchors and append parsed records
/// to `out`. Items are appended in the order encountered.
fn scan_page(body: &[u8], pn: u64, out: &mut Vec<LineItem>) {
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
        out.push(parse_anchor(body, pn, anchor));
    }
}

fn parse_anchor(body: &[u8], pn: u64, anchor_start: usize) -> LineItem {
    let id_start = anchor_start + 5;
    let invoice_id =
        std::str::from_utf8(&body[id_start..id_start + QB_ID_LEN])
            .expect("anchor guarded by is_base62")
            .to_owned();

    let payload_start = anchor_start + ANCHOR_LEN;
    let mut amount_raw = [0u8; 4];
    if payload_start + 4 <= body.len() {
        amount_raw.copy_from_slice(&body[payload_start..payload_start + 4]);
    }
    let amount_type = AmountType::from_byte(amount_raw[0]);
    let amount_cents = amount_type.decode_cents(&amount_raw);

    // Forward search for date+counter pair: SA date as u32 LE in 13000..20000,
    // followed by 4-byte counter > 0.
    let mut txn_date_raw = None;
    let mut counter = None;
    let end = (payload_start + 64).min(body.len().saturating_sub(8));
    for off in (payload_start + 4)..end {
        let d = u32::from_le_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        let c = u32::from_le_bytes([
            body[off + 4],
            body[off + 5],
            body[off + 6],
            body[off + 7],
        ]);
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
                item_qb_id = Some(
                    std::str::from_utf8(id_bytes).unwrap().to_owned(),
                );
            }
        }
    }

    LineItem {
        invoice_id,
        page_number: pn,
        page_offset: anchor_start,
        item_qb_id,
        amount_type,
        amount_cents,
        amount_raw,
        txn_date_raw,
        counter,
    }
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
    }

    #[test]
    fn base62_check() {
        assert!(is_base62(b"0000000000001QBm"));
        assert!(!is_base62(b"0000000000001QB!"));
        assert!(!is_base62(b"0000000000001 QB"));
    }

    #[test]
    fn scan_page_finds_synthetic_anchor() {
        // Craft a body containing one valid anchor at offset 32.
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        let anchor_offset = 32;
        // 5-byte parent prefix
        body[anchor_offset..anchor_offset + 5].copy_from_slice(&PARENT_PREFIX);
        // QB-ID
        body[anchor_offset + 5..anchor_offset + 5 + QB_ID_LEN]
            .copy_from_slice(b"0000000000001QBm");
        // float32(1.0)
        body[anchor_offset + 5 + QB_ID_LEN..anchor_offset + ANCHOR_LEN]
            .copy_from_slice(&F32_ONE);
        // amount bytes: type 0x02, cents 0x040F40 = 265536
        body[anchor_offset + ANCHOR_LEN..anchor_offset + ANCHOR_LEN + 4]
            .copy_from_slice(&[0x02, 0x40, 0x0F, 0x04]);
        // date u32 LE 15511, counter u32 LE 12671 at offset payload+8
        body[anchor_offset + ANCHOR_LEN + 8..anchor_offset + ANCHOR_LEN + 12]
            .copy_from_slice(&15511u32.to_le_bytes());
        body[anchor_offset + ANCHOR_LEN + 12..anchor_offset + ANCHOR_LEN + 16]
            .copy_from_slice(&12671u32.to_le_bytes());

        let mut out = Vec::new();
        scan_page(&body, 3679, &mut out);
        assert_eq!(out.len(), 1);
        let li = &out[0];
        assert_eq!(li.invoice_id, "0000000000001QBm");
        assert_eq!(li.amount_type, AmountType::Standard);
        assert_eq!(li.amount_cents, Some(266_048));
        assert_eq!(li.txn_date_raw, Some(15511));
        assert_eq!(li.counter, Some(12671));
    }
}
