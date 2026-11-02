//! Line-item record parsing.
//!
//! The lineitem record is anchored by the 25-byte pattern
//! `00 00 00 00 10 <16 ASCII base62 chars> 00 00 80 3F` where the 16 chars
//! are the parent transaction's QB-ID and the trailing bytes are a
//! `float32(1.0)` quantity. Immediately after the anchor sits a numeric
//! value field and a date/counter pair.
//!
//! The value field is the SQL Anywhere NUMERIC encoding, laid out as
//! `[k][ctrl][k digit bytes]`:
//!
//!   - `k` is the digit count, 1 to 10; the field is `2 + k` bytes wide
//!   - the digits are base 100, least significant first, each 0 to 99, so
//!     `coef = sum(d[i] * 100^i)`
//!   - `ctrl & 0x80` is the sign (set is negative) and `(ctrl & 0x7F) - 64`
//!     is the base-100 exponent `e`
//!   - the value is `+/- coef * 100^e`, kept exact as a [`Decimal`]
//!
//! `k = 0` (`00 81` in practice) is not decoded. A field whose digits, or
//! whose exponent outside -1..=2, do not fit the layout is not decoded
//! either; the item keeps its raw bytes and the reason in
//! [`LineItem::value_undecoded`]. A field whose length byte is the first or
//! last byte of a 512-byte sector, or whose bytes cross a sector boundary, is
//! not decoded (`sector_boundary`): each sector of a page has its own
//! keystream step, and a field spanning two cannot be shown to be read
//! correctly. A length byte above 10 elsewhere, or a field cut off by the
//! page end, is an [`Error`].
//!
//! The NUMERIC layout was worked out in issue #22.
//!
//! That the value is the line's amount is not established.

use std::ops::Range;

use opensqlany::{ApModel, PageStore, SECTOR_SIZE};

use crate::decimal::Decimal;
use crate::decode::{DecodedPage, Directory};
use crate::error::Error;
use crate::opaque::count_opaque_pages;
use crate::page_attribution::PageAttribution;
use crate::scan::{ReadOptions, RecordFlag, Scan, Warning, WarningKind, for_each_extent_page};

use crate::date::{DateScan, DateSource, sa_day_to_unix_day, scan_date_counter};

const PAGE_DATA_END: usize = 0xFF0;
const ANCHOR_LEN: usize = 25;
const QB_ID_LEN: usize = 16;
const F32_ONE: [u8; 4] = [0x00, 0x00, 0x80, 0x3F];
const PARENT_PREFIX: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x10];

/// Most digit bytes a value field holds.
const MAX_DIGITS: usize = 10;
/// Smallest base-100 exponent read as a value.
const MIN_EXPONENT: i32 = -1;
/// Largest base-100 exponent read as a value.
const MAX_EXPONENT: i32 = 2;
/// Bytes a record's date search skips after the anchor, whatever the width
/// of the value field.
const MIN_VALUE_SPAN: usize = 4;

/// Why a value field was kept as raw bytes and not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UndecodedReason {
    /// The digit count is 0 (`00 81` in practice); the field has no digits
    /// to read, and treating it as zero is not supported by the data.
    ZeroLength,
    /// A digit byte is above 99, so the bytes are not base-100 digits.
    DigitOver99,
    /// The exponent is outside -1..=2.
    ExponentOutOfRange,
    /// The length byte is the first or last byte of a 512-byte sector, or
    /// the field's bytes cross a sector boundary. Each sector has its own
    /// keystream step, so the field is not trusted; its raw bytes are kept.
    SectorBoundary,
}

impl UndecodedReason {
    /// Stable snake_case name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZeroLength => "zero_length",
            Self::DigitOver99 => "digit_over_99",
            Self::ExponentOutOfRange => "exponent_out_of_range",
            Self::SectorBoundary => "sector_boundary",
        }
    }
}

impl std::fmt::Display for UndecodedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value field as read from a page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ValueField {
    /// The field's bytes: `[k][ctrl][digits]`, or just `[0][ctrl]`.
    raw: Vec<u8>,
    /// The decoded value, or why it was not decoded.
    value: Result<Decimal, UndecodedReason>,
    /// `false` when the length byte could not be read as a digit count, so
    /// `raw` holds only `[k][ctrl]` and the field's real width is unknown.
    width_known: bool,
}

/// Read the value field at `at` in `body`. `anchor` is the offset of the
/// record's anchor, named in errors.
///
/// A field whose length byte is the first or last byte of a sector, or whose
/// bytes cross a sector boundary, is [`UndecodedReason::SectorBoundary`] and
/// not an error, whatever its length byte says.
fn read_value(body: &[u8], at: usize, pn: u64, anchor: usize) -> Result<ValueField, Error> {
    let truncated = || Error::TruncatedValue {
        page_number: pn,
        page_offset: anchor,
    };
    let (&k, &ctrl) = match (body.get(at), body.get(at + 1)) {
        (Some(k), Some(ctrl)) => (k, ctrl),
        _ => return Err(truncated()),
    };
    let k = k as usize;
    let at_sector_edge = at.is_multiple_of(SECTOR_SIZE) || (at + 1).is_multiple_of(SECTOR_SIZE);
    if k > MAX_DIGITS && at_sector_edge {
        return Ok(ValueField {
            raw: vec![k as u8, ctrl],
            value: Err(UndecodedReason::SectorBoundary),
            width_known: false,
        });
    }
    if k > MAX_DIGITS {
        return Err(Error::InvalidValueLength {
            page_number: pn,
            page_offset: anchor,
            length_byte: k as u8,
        });
    }
    let Some(raw) = body.get(at..at + 2 + k) else {
        return Err(truncated());
    };
    let raw = raw.to_vec();
    if at_sector_edge || at / SECTOR_SIZE != (at + 1 + k) / SECTOR_SIZE {
        return Ok(ValueField {
            raw,
            value: Err(UndecodedReason::SectorBoundary),
            width_known: true,
        });
    }
    if k == 0 {
        return Ok(ValueField {
            raw,
            value: Err(UndecodedReason::ZeroLength),
            width_known: true,
        });
    }
    let value = numeric_value(ctrl, &raw[2..]);
    Ok(ValueField {
        raw,
        value,
        width_known: true,
    })
}

/// The value of `digits` (base 100, least significant first) under the
/// control byte `ctrl`.
fn numeric_value(ctrl: u8, digits: &[u8]) -> Result<Decimal, UndecodedReason> {
    if digits.iter().any(|&d| d > 99) {
        return Err(UndecodedReason::DigitOver99);
    }
    let exponent = i32::from(ctrl & 0x7F) - 64;
    if !(MIN_EXPONENT..=MAX_EXPONENT).contains(&exponent) {
        return Err(UndecodedReason::ExponentOutOfRange);
    }
    // At most ten digits: the coefficient is below 10^20 and, at the largest
    // exponent, the value below 10^30, so i128 holds every case.
    let coef = digits
        .iter()
        .rev()
        .fold(0i128, |acc, &d| acc * 100 + i128::from(d));
    let sign = if ctrl & 0x80 != 0 { -1 } else { 1 };
    Ok(if exponent >= 0 {
        Decimal::new(sign * coef * 100i128.pow(exponent as u32), 0)
    } else {
        Decimal::new(sign * coef, exponent.unsigned_abs() * 2)
    })
}

/// Parsed line-item record.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LineItem {
    /// Parent transaction QB-ID (16 base62 characters).
    pub qb_id_parent: String,
    /// Source page number in the QBW file.
    pub page_number: u64,
    /// Byte offset within the decoded page body where the anchor starts.
    pub page_offset: usize,
    /// Item QB-ID - the line item's own identifier, when discovered.
    pub item_qb_id: Option<String>,
    /// The numeric value field, exact. `None` when the field was not decoded
    /// (see `value_undecoded`); callers must treat `None` as "unknown", not
    /// zero. That this value is the line's amount is not established.
    pub value: Option<Decimal>,
    /// Why `value` is `None`; `None` when it was decoded.
    pub value_undecoded: Option<UndecodedReason>,
    /// The bytes of the value field, `[k][ctrl][k digit bytes]` (just
    /// `[k][ctrl]` when `k` is 0).
    pub value_raw: Vec<u8>,
    /// SA17 transaction date (SA-day), set only when `date_source` is
    /// [`DateSource::WindowScan`].
    pub txn_date_raw: Option<u32>,
    /// SA17 transaction counter, set under the same condition.
    pub counter: Option<u32>,
    /// How `txn_date_raw` was found. `NotFound` and `Ambiguous` leave the
    /// date and counter unset.
    pub date_source: DateSource,
    /// Owning `SYSTABLE` name (e.g. `abmc_invoice_inventory_lineitem`) when
    /// the line item's page was attributable; `None` otherwise. The
    /// attribution is by page position only and is unverified: the
    /// content-based cross-check (`verify --strict-attribution`) agrees
    /// with it on a small share of pages.
    pub source_table: Option<String>,
    /// Reasons this record deserves less trust. Empty when no check flagged it.
    pub flags: Vec<RecordFlag>,
}

impl LineItem {
    /// Build a record with the given invoice id, source table and value, all
    /// other fields empty. For tests and tools that need to synthesize
    /// records; scans never call it.
    #[doc(hidden)]
    pub fn synthetic(
        qb_id_parent: &str,
        source_table: Option<&str>,
        value: Option<Decimal>,
    ) -> Self {
        Self {
            qb_id_parent: qb_id_parent.to_owned(),
            page_number: 0,
            page_offset: 0,
            item_qb_id: None,
            value,
            value_undecoded: value.is_none().then_some(UndecodedReason::ZeroLength),
            value_raw: Vec::new(),
            txn_date_raw: None,
            counter: None,
            date_source: DateSource::NotFound,
            source_table: source_table.map(str::to_owned),
            flags: Vec::new(),
        }
    }

    /// The transaction date as days since the Unix epoch (1970-01-01), with
    /// the crate's SA-day offset (see `txn_date_raw` for the stored value).
    /// The offset is fitted to one sample file and not confirmed against a
    /// date known from another source; a correction ships as a bug fix.
    /// `None` when the date is missing or outside the plausible range.
    pub fn txn_date_days_since_unix(&self) -> Option<i64> {
        self.txn_date_raw.and_then(sa_day_to_unix_day)
    }
}

/// Collect every line item in `store`, tagging each with the table its page
/// is attributed to (when `attribution` is given).
///
/// In strict mode the first undecodable record or page is returned as an
/// error. Under [`Leniency::RECORDS`](crate::Leniency::RECORDS) each is
/// listed in [`Scan::rejected`] and scanning continues. More than half of the
/// data pages being opaque is [`Error::ContentOpaque`] in both modes.
pub(crate) fn scan_lineitems(
    store: &PageStore,
    model: &ApModel,
    attribution: Option<&PageAttribution>,
    options: &ReadOptions,
) -> Result<Scan<LineItem>, Error> {
    let mut scan = Scan::default();
    let opaque = count_opaque_pages(store, model)?;
    if opaque.mostly_opaque() {
        return Err(Error::ContentOpaque {
            opaque: opaque.opaque,
            extent: opaque.extent,
        });
    }
    if opaque.opaque > 0 {
        scan.warnings.push(Warning::new(WarningKind::OpaquePages {
            opaque: opaque.opaque,
            extent: opaque.extent,
        }));
    }
    let (mut outside_live_rows, mut other_quantity, mut sector_boundary_anchors) =
        (0u64, 0u64, 0u64);
    for_each_extent_page(
        store,
        model,
        options,
        &mut scan,
        |_| true,
        |decoded, scan| {
            let source = attribution
                .and_then(|a| a.attribute(decoded.pn()))
                .map(|e| e.name.clone());
            let found = page_lineitems(decoded, source)?;
            outside_live_rows += found.outside_live_rows;
            other_quantity += found.other_quantity;
            sector_boundary_anchors += found.sector_boundary;
            for r in found.records {
                match r {
                    Ok(li) => scan.items.push(li),
                    Err(e) => scan.reject(e, options)?,
                }
            }
            Ok(())
        },
    )?;
    if outside_live_rows > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::RecordsOutsideLiveRows {
                records: outside_live_rows,
            }));
    }
    if other_quantity > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::LineItemsNotRead {
                anchors: other_quantity,
            }));
    }
    if sector_boundary_anchors > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::SectorBoundaryAnchors {
                anchors: sector_boundary_anchors,
            }));
    }
    let mut undecoded = [0u64; 4];
    for reason in scan.items.iter().filter_map(|li| li.value_undecoded) {
        match reason {
            UndecodedReason::ZeroLength => undecoded[0] += 1,
            UndecodedReason::DigitOver99 => undecoded[1] += 1,
            UndecodedReason::ExponentOutOfRange => undecoded[2] += 1,
            UndecodedReason::SectorBoundary => undecoded[3] += 1,
        }
    }
    if undecoded.iter().any(|&n| n > 0) {
        scan.warnings
            .push(Warning::new(WarningKind::UndecodedValues {
                zero_length: undecoded[0],
                digit_over_99: undecoded[1],
                exponent_out_of_range: undecoded[2],
                sector_boundary: undecoded[3],
            }));
    }
    let unattributed = scan
        .items
        .iter()
        .filter(|li| li.source_table.is_none())
        .count() as u64;
    if attribution.is_some() && unattributed > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::UnattributedRecords {
                records: unattributed,
                total: scan.items.len() as u64,
            }));
    }
    Ok(scan)
}

/// Line items found on one decoded page.
struct PageLineItems {
    /// Records and record errors, in page order.
    records: Vec<Result<LineItem, Error>>,
    /// Anchors left out because they lie outside every live row.
    outside_live_rows: u64,
    /// Anchor-shaped records with a quantity other than 1.0, not parsed.
    other_quantity: u64,
    /// Anchors cut by a sector boundary that could not be read.
    sector_boundary: u64,
}

/// What a page scan found besides the parsed records.
#[derive(Debug, Default)]
struct PageScan {
    /// Offsets of anchor-shaped records whose quantity is a plausible value
    /// other than 1.0.
    other_quantity: Vec<usize>,
    /// Offsets of anchors cut by a sector boundary: the prefix and ID
    /// characters before the boundary match, but the whole anchor does not.
    sector_boundary: Vec<usize>,
}

/// Parse the line items on `decoded`, flag them for the page's decode
/// confidence, and attribute them to `source`.
///
/// When the page has a confirmed slot directory, a live row runs from its
/// slot offset to the next live offset (or the page trailer). An anchor
/// outside every such span lies in the page prelude, the directory or the
/// free space below the lowest live row: it is the residue of a deleted or
/// moved record, so it is counted and left out. Pages without a confirmed
/// directory keep every anchor, because their row extents are unknown.
///
/// opensqlany does not decode row lengths, so a deleted row that sits inside
/// a live row's span cannot be told from live data.
fn page_lineitems(decoded: &DecodedPage, source: Option<String>) -> Result<PageLineItems, Error> {
    let live_spans: Option<Vec<Range<usize>>> = match decoded.slotted()? {
        Directory::Confirmed(sp) => Some(
            sp.rows()
                .iter()
                .map(|r| r.offset() as usize..r.offset() as usize + r.bytes().len())
                .collect(),
        ),
        _ => None,
    };
    let is_live = |offset: usize| {
        live_spans
            .as_ref()
            .is_none_or(|spans| spans.iter().any(|s| s.contains(&offset)))
    };
    let mut found = Vec::new();
    let page_scan = scan_page(&decoded.bytes()[..PAGE_DATA_END], decoded.pn(), &mut found);
    let mut out = PageLineItems {
        records: Vec::with_capacity(found.len()),
        outside_live_rows: 0,
        other_quantity: 0,
        sector_boundary: 0,
    };
    for offset in page_scan.other_quantity {
        if is_live(offset) {
            out.other_quantity += 1;
        } else {
            out.outside_live_rows += 1;
        }
    }
    for offset in page_scan.sector_boundary {
        if is_live(offset) {
            out.sector_boundary += 1;
        } else {
            out.outside_live_rows += 1;
        }
    }
    for r in found {
        let mut li = match r {
            Ok(li) => li,
            Err(e) => {
                out.records.push(Err(e));
                continue;
            }
        };
        if !is_live(li.page_offset) {
            out.outside_live_rows += 1;
            continue;
        }
        // A record reads up to 96 bytes before its anchor (item ID) and 64
        // bytes after its payload (date); flag it when any of those bytes
        // came from an unconfirmed keystream or a weak sector.
        let span = li.page_offset.saturating_sub(96)
            ..(li.page_offset + ANCHOR_LEN + 64).min(PAGE_DATA_END);
        li.flags.extend(decoded.flags_for(span));
        li.source_table = source.clone();
        out.records.push(Ok(li));
    }
    Ok(out)
}

/// Scan a decoded page body for line-item anchors and append parsed records
/// to `out` in page order.
///
/// Also returns the offsets of anchors whose quantity position holds a
/// plausible quantity other than 1.0 (see [`plausible_quantity`]); they are
/// not parsed. A candidate whose bytes overlap a parsed anchor is part of
/// that record and is not returned. The same goes for anchors cut by a
/// sector boundary (see [`PageScan::sector_boundary`]).
fn scan_page(body: &[u8], pn: u64, out: &mut Vec<Result<LineItem, Error>>) -> PageScan {
    let mut other_quantity = Vec::new();
    if body.len() < ANCHOR_LEN {
        return PageScan::default();
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
        // Verify trailing float32(1.0). Any other plausible quantity is
        // counted: the record may be a real line this decoder cannot read.
        let qty = &body[pos + 5 + QB_ID_LEN..pos + ANCHOR_LEN];
        if qty != F32_ONE {
            let q = f32::from_le_bytes([qty[0], qty[1], qty[2], qty[3]]);
            if plausible_quantity(q)
                && other_quantity
                    .last()
                    .is_none_or(|&last| pos - last >= ANCHOR_LEN)
            {
                other_quantity.push(pos);
            }
            pos += 1;
            continue;
        }
        start.push(pos);
        pos += ANCHOR_LEN;
    }

    other_quantity.retain(|&c| start.iter().all(|&a| c.abs_diff(a) >= ANCHOR_LEN));
    let mut sector_boundary = cut_anchors(body);
    sector_boundary.retain(|&c| start.iter().all(|&a| c.abs_diff(a) >= ANCHOR_LEN));

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
    for (anchor, floor, ceil) in bounds {
        out.push(parse_anchor(body, pn, anchor, floor, ceil));
    }
    PageScan {
        other_quantity,
        sector_boundary,
    }
}

/// Offsets where an anchor would start in the last 24 bytes before a sector
/// boundary (in-sector offsets 488 to 507): the 5-byte prefix matches and so
/// does every ID character that lies before the boundary (all 16 from 488 to
/// 491, fewer after), so only the boundary hides the rest. The bytes after
/// the boundary are not examined, and neither are quantity bytes before it.
/// Such an anchor is not read. A prefix the boundary itself cuts (offsets
/// 508 to 511) shows one to four zero bytes and no ID character, which
/// matches too much padding to count, so those anchors are not counted.
///
/// The boundary inside the page body counts; the page's last sector ends
/// at the trailer, past the body, so it has none.
fn cut_anchors(body: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    for boundary in (SECTOR_SIZE..body.len()).step_by(SECTOR_SIZE) {
        for pos in boundary - ANCHOR_LEN + 1..=boundary - 5 {
            // The ID characters end at pos + 21; the quantity bytes after
            // them are not examined.
            let id_end = boundary.min(pos + 5 + QB_ID_LEN);
            if body[pos..pos + 5] == PARENT_PREFIX && is_base62(&body[pos + 5..id_end]) {
                found.push(pos);
            }
        }
    }
    found
}

/// Whether `q`, read from the quantity position of an anchor-shaped record,
/// is a quantity a line could carry: zero, or a magnitude from 0.001 to
/// 1,000,000, either sign (returns and credits are negative). The bounds
/// are a plausibility choice of this crate, not a format rule; anything else
/// (NaN, infinities, denormals, huge values) is taken as other bytes.
fn plausible_quantity(q: f32) -> bool {
    q == 0.0 || (q.is_finite() && (1.0e-3..=1.0e6).contains(&q.abs()))
}

fn parse_anchor(
    body: &[u8],
    pn: u64,
    anchor_start: usize,
    floor: usize,
    ceil: usize,
) -> Result<LineItem, Error> {
    let id_start = anchor_start + 5;
    let qb_id_parent = std::str::from_utf8(&body[id_start..id_start + QB_ID_LEN])
        .expect("anchor guarded by is_base62")
        .to_owned();

    let payload_start = anchor_start + ANCHOR_LEN;
    let field = read_value(body, payload_start, pn, anchor_start)?;

    // Forward search for a date+counter pair after the value field.
    // When the length byte was unreadable the field's width is unknown, and
    // digit bytes could pass for a date: look for none.
    let scan = if field.width_known {
        scan_date_counter(
            body,
            payload_start + field.raw.len().max(MIN_VALUE_SPAN),
            (payload_start + 64).min(ceil.saturating_sub(7)),
            1_000_000,
        )
    } else {
        DateScan {
            date: None,
            counter: None,
            source: DateSource::NotFound,
        }
    };

    // Backward search for the nearest item QB-ID prefixed by `04 00 10`,
    // not reaching into the previous record.
    let back_start = anchor_start.saturating_sub(96).max(floor).min(anchor_start);
    let item_qb_id = find_item_id_nearest(&body[back_start..anchor_start]);

    let mut li = LineItem {
        qb_id_parent,
        page_number: pn,
        page_offset: anchor_start,
        item_qb_id,
        value: field.value.ok(),
        value_undecoded: field.value.err(),
        value_raw: field.raw,
        txn_date_raw: scan.date,
        counter: scan.counter,
        date_source: scan.source,
        source_table: None,
        flags: Vec::new(),
    };
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

    /// Decode `payload` as a value field in the middle of a page.
    fn value_of(payload: &[u8]) -> Result<ValueField, Error> {
        let mut body = [0u8; 64];
        let end = payload.len().min(body.len() - 8);
        body[8..8 + end].copy_from_slice(&payload[..end]);
        read_value(&body[..8 + payload.len()], 8, 7, 0)
    }

    fn decoded(payload: &[u8]) -> String {
        match value_of(payload).unwrap().value {
            Ok(v) => v.to_string(),
            Err(r) => format!("undecoded: {r}"),
        }
    }

    #[test]
    fn issue_22_examples_decode() {
        assert_eq!(decoded(&[0x02, 0x40, 0x08, 0x02]), "208");
        assert_eq!(decoded(&[0x01, 0x41, 0x03]), "300");
        assert_eq!(decoded(&[0x02, 0x3F, 0x2F, 0x4B]), "75.47");
    }

    #[test]
    fn real_data_shapes_decode() {
        assert_eq!(decoded(&[0x02, 0xBF, 0x2D, 0x22]), "-34.45");
        assert_eq!(decoded(&[0x03, 0x3F, 0x39, 0x56, 0x2F]), "4786.57");
        assert_eq!(decoded(&[0x02, 0xC0, 0x3C, 0x09]), "-960");
        assert_eq!(decoded(&[0x01, 0x41, 0x12]), "1800");
        assert_eq!(decoded(&[0x03, 0xBF, 0x32, 0x31, 0x03]), "-349.5");
    }

    #[test]
    fn field_width_is_two_plus_the_digit_count() {
        let f = value_of(&[0x03, 0x3F, 0x39, 0x56, 0x2F]).unwrap();
        assert_eq!(f.raw, [0x03, 0x3F, 0x39, 0x56, 0x2F]);
        let f = value_of(&[0x02, 0x40, 0x08, 0x02, 0xEE, 0xEE]).unwrap();
        assert_eq!(f.raw, [0x02, 0x40, 0x08, 0x02]);
    }

    #[test]
    fn extreme_valid_fields_are_exact() {
        // Ten 99 digits at the largest exponent.
        let mut p = vec![0x0A, 0x40 + 2];
        p.extend([99u8; 10]);
        assert_eq!(decoded(&p), "99999999999999999999".to_owned() + "0000");
        // Smallest exponent: two decimal places.
        assert_eq!(decoded(&[0x01, 0x40 - 1, 0x01]), "0.01");
        assert_eq!(decoded(&[0x01, 0xC0 - 1, 0x01]), "-0.01");
    }

    #[test]
    fn a_zero_coefficient_is_zero_whatever_the_sign() {
        assert_eq!(decoded(&[0x01, 0x40, 0x00]), "0");
        assert_eq!(decoded(&[0x02, 0xC0, 0x00, 0x00]), "0");
    }

    #[test]
    fn zero_length_field_is_undecoded_and_keeps_its_bytes() {
        let f = value_of(&[0x00, 0x81, 0x07, 0x07]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::ZeroLength));
        assert_eq!(f.raw, [0x00, 0x81]);
    }

    #[test]
    fn digit_above_99_is_undecoded() {
        let f = value_of(&[0x02, 0x40, 0x08, 0x64]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::DigitOver99));
        assert_eq!(f.raw, [0x02, 0x40, 0x08, 0x64]);
    }

    #[test]
    fn exponent_outside_range_is_undecoded() {
        // 0x40 - 2 is e = -2; 0x40 + 3 is e = 3; 0x80 and 0x00 are -64.
        // 0xC5 is the -23 x 100^5 field seen once on the Rock Castle sample.
        for ctrl in [0x3Eu8, 0x43, 0xBE, 0xC3, 0x3D, 0xC5, 0x80, 0x00, 0x7F] {
            let f = value_of(&[0x01, ctrl, 0x01]).unwrap();
            assert_eq!(
                f.value,
                Err(UndecodedReason::ExponentOutOfRange),
                "{ctrl:#x}"
            );
        }
        for ctrl in [0x3Fu8, 0x42, 0xBF, 0xC2] {
            assert!(
                value_of(&[0x01, ctrl, 0x01]).unwrap().value.is_ok(),
                "{ctrl:#x}"
            );
        }
    }

    #[test]
    fn length_above_ten_is_an_error() {
        // 0x04 and the lengths up to ten are digit counts now.
        assert!(value_of(&[0x04, 0x40, 1, 2, 3, 4]).unwrap().value.is_ok());
        let mut p = vec![0x0B, 0x40];
        p.extend([1u8; 11]);
        assert!(matches!(
            value_of(&p).unwrap_err(),
            Error::InvalidValueLength {
                page_number: 7,
                page_offset: 0,
                length_byte: 0x0B
            }
        ));
    }

    #[test]
    fn field_cut_off_by_the_page_end_is_an_error() {
        for payload in [&[][..], &[0x02][..], &[0x02, 0x40, 0x08][..], &[0x00][..]] {
            assert!(
                matches!(value_of(payload).unwrap_err(), Error::TruncatedValue { .. }),
                "{payload:?}"
            );
        }
        // A zero-length field needs only its two bytes.
        assert!(value_of(&[0x00, 0x81]).is_ok());
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
    fn anchors_less_than_an_anchor_apart_do_not_panic() {
        // The second anchor starts less than 29 + 4 bytes after the
        // first, so its item-ID search floor lies past the anchor itself.
        let mut body = anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x0F, 0x04]);
        let second = 32 + ANCHOR_LEN;
        body[second..second + 5].copy_from_slice(&PARENT_PREFIX);
        body[second + 5..second + 5 + QB_ID_LEN].copy_from_slice(b"0000000000002QBm");
        body[second + 5 + QB_ID_LEN..second + ANCHOR_LEN].copy_from_slice(&F32_ONE);
        body[second + ANCHOR_LEN..second + ANCHOR_LEN + 4]
            .copy_from_slice(&[0x02, 0x40, 0x0F, 0x04]);
        let mut out = Vec::new();
        scan_page(&body, 7, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn scan_page_finds_synthetic_anchor() {
        let mut body = anchor_body(PAGE_DATA_END, 32, &[0x02, 0x40, 0x0F, 0x04]);
        // date u32 LE 15511, counter u32 LE 12671 at offset payload+8
        let p = 32 + ANCHOR_LEN;
        body[p + 8..p + 12].copy_from_slice(&15511u32.to_le_bytes());
        body[p + 12..p + 16].copy_from_slice(&12671u32.to_le_bytes());

        let li = scan_one(&body).unwrap();
        assert_eq!(li.qb_id_parent, "0000000000001QBm");
        assert_eq!(li.value_raw, [0x02, 0x40, 0x0F, 0x04]);
        assert_eq!(li.value.unwrap().to_string(), "415");
        assert_eq!(li.value_undecoded, None);
        assert_eq!(li.txn_date_raw, Some(15511));
        assert_eq!(li.counter, Some(12671));
        assert_eq!(li.date_source, DateSource::WindowScan);
        // No attribution is needed to decode the value.
        assert_eq!(li.source_table, None);
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
        assert_eq!(ids, ["AAAAAAAAAAAAAAAA", "BBBBBBBBBBBBBBBB"]);
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
        assert_eq!(out[1].as_ref().unwrap().item_qb_id, None);
        assert_eq!(
            out[0].as_ref().unwrap().item_qb_id.as_deref(),
            Some("AAAAAAAAAAAAAAAA")
        );
    }

    #[test]
    fn invalid_value_length_is_an_error() {
        let err = scan_one(&anchor_body(PAGE_DATA_END, 32, &[0x5A, 0x01, 0x02, 0x03])).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidValueLength {
                page_number: 7,
                page_offset: 32,
                length_byte: 0x5A
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
        assert_eq!(li.value_raw, [0x02, 0x40, 0x0F, 0x04]);
    }

    #[test]
    fn anchor_with_cut_off_amount_is_an_error() {
        let err = scan_one(&anchor_body(
            PAGE_DATA_END,
            PAGE_DATA_END - ANCHOR_LEN - 2,
            &[0x02, 0x40],
        ))
        .unwrap_err();
        assert!(matches!(err, Error::TruncatedValue { .. }));
    }

    /// A plaintext page whose prelude-confirmed slot directory at 0x100 has
    /// four live rows at 0xC00..0xF00, leaving 0x108..0xC00 as free space.
    fn slotted_page() -> Vec<u8> {
        let mut page = vec![0u8; 4096];
        page[0xFC..0xFE].copy_from_slice(&4u16.to_le_bytes());
        for (k, off) in [0xF00u16, 0xE00, 0xD00, 0xC00].iter().enumerate() {
            page[0x100 + 2 * k..0x102 + 2 * k].copy_from_slice(&off.to_le_bytes());
        }
        page[PAGE_DATA_END] = 4;
        page
    }

    fn put_anchor(page: &mut [u8], at: usize) {
        page[at..at + 5].copy_from_slice(&PARENT_PREFIX);
        page[at + 5..at + 21].copy_from_slice(b"0000000000001QBm");
        page[at + 21..at + 25].copy_from_slice(&F32_ONE);
        page[at + 25..at + 29].copy_from_slice(&[0x02, 0x40, 0x0F, 0x04]);
    }

    #[test]
    fn anchor_in_free_space_is_not_a_live_record() {
        // S1-6: bytes between the slot directory and the lowest live row
        // are free space; an anchor there is a deleted or stale record.
        let mut page = slotted_page();
        put_anchor(&mut page, 0x800);
        put_anchor(&mut page, 0xC10);
        let decoded = DecodedPage::from_plain(9, page);
        let found = page_lineitems(&decoded, None).unwrap();
        let offsets: Vec<usize> = found
            .records
            .iter()
            .map(|r| r.as_ref().unwrap().page_offset)
            .collect();
        assert_eq!(offsets, [0xC10]);
        assert_eq!(found.outside_live_rows, 1);
    }

    #[test]
    fn anchor_with_another_quantity_is_counted() {
        // S2-16: the anchor requires a float32 1.0 in the position the
        // module docs call the quantity. A line with quantity 3 would be
        // silently absent; it is now counted.
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        page[0x800 + 21..0x800 + 25].copy_from_slice(&3.0f32.to_le_bytes());
        let decoded = DecodedPage::from_plain(9, page);
        let found = page_lineitems(&decoded, None).unwrap();
        assert!(found.records.is_empty());
        assert_eq!(found.other_quantity, 1);
    }

    fn offsets(found: &PageLineItems) -> Vec<usize> {
        found
            .records
            .iter()
            .map(|r| r.as_ref().unwrap().page_offset)
            .collect()
    }

    fn put_quantity(page: &mut [u8], anchor: usize, q: f32) {
        page[anchor + 21..anchor + 25].copy_from_slice(&q.to_le_bytes());
    }

    #[test]
    fn anchor_before_the_directory_is_not_a_live_record() {
        // Bytes below the lowest live row are outside every row, including
        // the page prelude that precedes the slot directory.
        let mut page = slotted_page();
        put_anchor(&mut page, 0x20);
        put_anchor(&mut page, 0xC10);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(offsets(&found), [0xC10]);
        assert_eq!(found.outside_live_rows, 1);
    }

    #[test]
    fn zero_and_negative_quantities_are_counted() {
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        put_quantity(&mut page, 0x800, 0.0);
        put_anchor(&mut page, 0x900);
        put_quantity(&mut page, 0x900, -2.0);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert!(found.records.is_empty());
        assert_eq!(found.other_quantity, 2);
    }

    #[test]
    fn implausible_quantity_bytes_are_not_counted() {
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        put_quantity(&mut page, 0x800, 5.0e8);
        put_anchor(&mut page, 0x900);
        put_quantity(&mut page, 0x900, 1.0e-30);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(found.other_quantity, 0);
    }

    #[test]
    fn other_quantity_anchor_outside_live_rows_is_not_counted_as_unread() {
        let mut page = slotted_page();
        put_anchor(&mut page, 0x800);
        put_quantity(&mut page, 0x800, 3.0);
        put_anchor(&mut page, 0xC10);
        put_quantity(&mut page, 0xC10, 3.0);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(found.other_quantity, 1);
        assert_eq!(found.outside_live_rows, 1);
    }

    #[test]
    fn candidate_overlapping_a_record_is_not_counted() {
        // The candidate's zero quantity is the start of the real anchor that
        // follows it.
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        put_quantity(&mut page, 0x800, 0.0);
        put_anchor(&mut page, 0x800 + 21);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(offsets(&found), [0x800 + 21]);
        assert_eq!(found.other_quantity, 0);
    }

    #[test]
    fn anchors_a_few_bytes_apart_are_both_read() {
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        put_anchor(&mut page, 0x800 + ANCHOR_LEN);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(offsets(&found), [0x800, 0x800 + ANCHOR_LEN]);
    }

    #[test]
    fn page_without_directory_keeps_every_anchor() {
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 0x800);
        let decoded = DecodedPage::from_plain(9, page);
        let found = page_lineitems(&decoded, None).unwrap();
        assert_eq!(found.records.len(), 1);
        assert_eq!(found.outside_live_rows, 0);
    }

    #[test]
    fn unattributed_line_items_are_counted() {
        // S2-20: when attribution covers only part of the file, line items
        // on the other pages have no source table and no decoded amount; the
        // scan says how many.
        let mut plain = vec![0u8; 4096];
        put_anchor(&mut plain, 0x800);
        plain[0xF00..0xF04].copy_from_slice(&[0x00, 0x00, 0xD5, 0x0B]);
        plain[0xFF2] = b'E';
        let bias = 0u8; // page 1
        for (i, b) in plain[..PAGE_DATA_END].iter_mut().enumerate() {
            *b = b.wrapping_add(0x5Au8.wrapping_add(1 + (i / 512) as u8).wrapping_sub(bias));
        }
        let mut bytes = vec![0u8; 4096];
        bytes.extend(plain);
        let store = crate::testutil::sealed_store(bytes);
        let model = crate::testutil::lenient_model(&store);
        let attribution = PageAttribution::from_catalog(Vec::new());
        let scan =
            scan_lineitems(&store, &model, Some(&attribution), &ReadOptions::strict()).unwrap();
        assert_eq!(scan.items.len(), 1);
        assert!(scan.warnings.iter().any(|w| w.kind
            == WarningKind::UnattributedRecords {
                records: 1,
                total: 1
            }));
    }

    #[test]
    fn undecoded_values_are_counted_by_reason() {
        // Three fields that cannot be decoded, one of each reason, and one
        // that can: the scan counts each reason.
        let mut plain = vec![0u8; 4096];
        for (at, payload) in [
            (0x200, &[0x00u8, 0x81, 0x00, 0x00][..]),
            (0x400, &[0x02, 0x40, 0x08, 0x64][..]),
            (0x600, &[0x01, 0x80, 0x01][..]),
            (0x800, &[0x02, 0x40, 0x0F, 0x04][..]),
        ] {
            put_anchor(&mut plain, at);
            plain[at + 25..at + 25 + payload.len()].copy_from_slice(payload);
        }
        plain[0xF00..0xF04].copy_from_slice(&[0x00, 0x00, 0xD5, 0x0B]);
        plain[0xFF2] = b'E';
        let bias = 0u8; // page 1
        for (i, b) in plain[..PAGE_DATA_END].iter_mut().enumerate() {
            *b = b.wrapping_add(0x5Au8.wrapping_add(1 + (i / 512) as u8).wrapping_sub(bias));
        }
        let mut bytes = vec![0u8; 4096];
        bytes.extend(plain);
        let store = crate::testutil::sealed_store(bytes);
        let model = crate::testutil::lenient_model(&store);
        let attribution = PageAttribution::from_catalog(Vec::new());
        let scan =
            scan_lineitems(&store, &model, Some(&attribution), &ReadOptions::strict()).unwrap();
        assert_eq!(scan.items.len(), 4);
        let reasons: Vec<_> = scan.items.iter().map(|li| li.value_undecoded).collect();
        assert_eq!(
            reasons,
            [
                Some(UndecodedReason::ZeroLength),
                Some(UndecodedReason::DigitOver99),
                Some(UndecodedReason::ExponentOutOfRange),
                None
            ]
        );
        assert!(scan.warnings.iter().any(|w| w.kind
            == WarningKind::UndecodedValues {
                zero_length: 1,
                digit_over_99: 1,
                exponent_out_of_range: 1,
                sector_boundary: 0
            }));
    }

    /// Body of two sectors (1024 bytes) with `payload` at `at`.
    fn two_sector_value(at: usize, payload: &[u8]) -> Result<ValueField, Error> {
        let mut body = vec![0u8; 1024];
        body[at..at + payload.len()].copy_from_slice(payload);
        read_value(&body, at, 7, at - 25)
    }

    #[test]
    fn length_byte_at_a_sector_start_is_undecoded_not_an_error() {
        let f = two_sector_value(512, &[0x30, 0x40, 0x01]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::SectorBoundary));
        assert_eq!(f.raw, [0x30, 0x40]);
        assert!(!f.width_known);
        // A plausible length at the start of a sector is not trusted either.
        let f = two_sector_value(512, &[0x02, 0x40, 0x08, 0x02]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::SectorBoundary));
        assert_eq!(f.raw, [0x02, 0x40, 0x08, 0x02]);
        assert!(f.width_known);
    }

    #[test]
    fn length_byte_in_a_sector_end_is_undecoded() {
        let f = two_sector_value(511, &[0x77, 0x40]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::SectorBoundary));
        assert_eq!(f.raw, [0x77, 0x40]);
    }

    #[test]
    fn value_crossing_a_sector_boundary_is_undecoded_with_its_bytes() {
        let f = two_sector_value(509, &[0x03, 0x3F, 0x39, 0x56, 0x2F]).unwrap();
        assert_eq!(f.value, Err(UndecodedReason::SectorBoundary));
        assert_eq!(f.raw, [0x03, 0x3F, 0x39, 0x56, 0x2F]);
        assert!(f.width_known);
    }

    #[test]
    fn value_ending_at_the_sector_end_is_decoded() {
        // 507..512 holds [k][ctrl][3 digits]: the last digit is the last byte
        // of the sector.
        let f = two_sector_value(507, &[0x03, 0x3F, 0x39, 0x56, 0x2F]).unwrap();
        assert_eq!(f.value.unwrap().to_string(), "4786.57");
    }

    #[test]
    fn bad_length_byte_away_from_a_boundary_is_still_an_error() {
        assert!(matches!(
            two_sector_value(600, &[0x30, 0x40]).unwrap_err(),
            Error::InvalidValueLength { .. }
        ));
    }

    #[test]
    fn record_with_unreadable_length_byte_at_a_boundary_has_no_guessed_date() {
        // Anchor ends exactly on the sector boundary; the length byte is
        // junk. A date-like pair sits where a value this wide could not
        // reach: nothing may be taken from it.
        let mut body = anchor_body(1024, 487, &[0x30, 0x40, 0x01, 0x02]);
        body[512 + 8..512 + 12].copy_from_slice(&23_817u32.to_le_bytes());
        body[512 + 12..512 + 16].copy_from_slice(&500u32.to_le_bytes());
        let li = scan_one(&body).unwrap();
        assert_eq!(li.value, None);
        assert_eq!(li.value_undecoded, Some(UndecodedReason::SectorBoundary));
        assert_eq!(li.value_raw, [0x30, 0x40]);
        assert_eq!(li.date_source, DateSource::NotFound);
        assert_eq!(li.txn_date_raw, None);
        assert_eq!(li.counter, None);
    }

    #[test]
    fn straddling_value_with_a_known_width_still_finds_its_date() {
        // Anchor ends at 509, so the value's last bytes are in sector 1.
        let mut body = anchor_body(1024, 484, &[0x02, 0x40, 0x0F, 0x04]);
        let p = 484 + ANCHOR_LEN;
        body[p + 8..p + 12].copy_from_slice(&23_817u32.to_le_bytes());
        body[p + 12..p + 16].copy_from_slice(&500u32.to_le_bytes());
        let li = scan_one(&body).unwrap();
        assert_eq!(li.value_undecoded, Some(UndecodedReason::SectorBoundary));
        assert_eq!(li.txn_date_raw, Some(23_817));
    }

    #[test]
    fn anchor_cut_by_a_sector_boundary_is_counted_not_read() {
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        // Prefix and part of the ID before the boundary at 512; nothing
        // usable after it.
        for start in [490usize, 1024 + 500] {
            let boundary = (start / 512 + 1) * 512;
            body[start..start + 5].copy_from_slice(&PARENT_PREFIX);
            for b in &mut body[start + 5..boundary] {
                *b = b'Q';
            }
        }
        let mut out = Vec::new();
        let found = scan_page(&body, 7, &mut out);
        assert!(out.is_empty());
        assert_eq!(found.sector_boundary, [490, 1524]);
    }

    #[test]
    fn anchor_whose_id_is_whole_but_quantity_is_cut_is_counted() {
        // At in-sector offsets 488 to 490 the 16 ID characters end before
        // the boundary and the quantity bytes that follow them are not ID
        // characters.
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        for start in [488usize, 1024 + 489, 2048 + 490] {
            body[start..start + 5].copy_from_slice(&PARENT_PREFIX);
            for b in &mut body[start + 5..start + 21] {
                *b = b'Q';
            }
        }
        let mut out = Vec::new();
        let found = scan_page(&body, 7, &mut out);
        assert!(out.is_empty());
        assert_eq!(found.sector_boundary, [488, 1513, 2538]);
    }

    #[test]
    fn prefix_cut_by_the_boundary_is_not_counted() {
        // From offset 508 on, the boundary hides the end of the prefix and
        // every ID character; zero bytes alone are not an anchor.
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        for start in [508usize, 1024 + 509, 2048 + 510, 3072 + 511] {
            let boundary = (start / 512 + 1) * 512;
            let visible = boundary - start;
            body[start..boundary].copy_from_slice(&PARENT_PREFIX[..visible]);
        }
        let mut out = Vec::new();
        let found = scan_page(&body, 7, &mut out);
        assert!(found.sector_boundary.is_empty());
    }

    #[test]
    fn whole_anchor_across_a_boundary_is_read_not_counted_as_cut() {
        let body = anchor_body(PAGE_DATA_END, 495, &[0x02, 0x40, 0x0F, 0x04]);
        let mut out = Vec::new();
        let found = scan_page(&body, 7, &mut out);
        assert_eq!(out.len(), 1);
        assert!(found.sector_boundary.is_empty());
    }

    #[test]
    fn sector_boundary_values_are_counted_by_reason() {
        let mut page = vec![0u8; 4096];
        put_anchor(&mut page, 487);
        page[512..514].copy_from_slice(&[0x30, 0x40]);
        put_anchor(&mut page, 0x800);
        let found = page_lineitems(&DecodedPage::from_plain(9, page), None).unwrap();
        assert_eq!(found.records.len(), 2);
        assert_eq!(
            found.records[0].as_ref().unwrap().value_undecoded,
            Some(UndecodedReason::SectorBoundary)
        );
    }

    #[test]
    fn sector_boundary_values_and_cut_anchors_are_warned_about() {
        let mut plain = vec![0u8; 4096];
        // Value field crossing the boundary at 1024.
        put_anchor(&mut plain, 1024 - ANCHOR_LEN - 2);
        plain[0xF00..0xF04].copy_from_slice(&[0x00, 0x00, 0xD5, 0x0B]);
        // Anchor cut by the boundary at 2048.
        let cut = 2048 - 15;
        plain[cut..cut + 5].copy_from_slice(&PARENT_PREFIX);
        plain[cut + 5..2048].fill(b'Q');
        plain[0xFF2] = b'E';
        for (i, b) in plain[..PAGE_DATA_END].iter_mut().enumerate() {
            *b = b.wrapping_add(0x5Au8.wrapping_add(1 + (i / 512) as u8));
        }
        let mut bytes = vec![0u8; 4096];
        bytes.extend(plain);
        let store = crate::testutil::sealed_store(bytes);
        let model = crate::testutil::lenient_model(&store);
        let attribution = PageAttribution::from_catalog(Vec::new());
        let scan =
            scan_lineitems(&store, &model, Some(&attribution), &ReadOptions::strict()).unwrap();
        assert_eq!(scan.items.len(), 1);
        assert_eq!(
            scan.items[0].value_undecoded,
            Some(UndecodedReason::SectorBoundary)
        );
        assert!(scan.warnings.iter().any(|w| w.kind
            == WarningKind::UndecodedValues {
                zero_length: 0,
                digit_over_99: 0,
                exponent_out_of_range: 0,
                sector_boundary: 1
            }));
        assert!(
            scan.warnings
                .iter()
                .any(|w| w.kind == WarningKind::SectorBoundaryAnchors { anchors: 1 })
        );
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
        crate::testutil::sealed_store(b)
    }

    #[test]
    fn mostly_opaque_file_is_an_error() {
        let store = random_extent_store(11, 8);
        let model = crate::testutil::lenient_model(&store);
        let err = scan_lineitems(&store, &model, None, &ReadOptions::strict()).unwrap_err();
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
        let model = crate::testutil::lenient_model(&store);
        let scan = scan_lineitems(&store, &model, None, &ReadOptions::strict()).unwrap();
        assert!(matches!(
            scan.warnings.as_slice(),
            [
                Warning {
                    kind: WarningKind::OpaquePages {
                        opaque: 2,
                        extent: 10
                    }
                },
                ..
            ]
        ));
    }
}
