//! Minimal transaction-header record scanner.
//!
//! QB transaction-header tables (`abmc_*_header`) store one row per
//! transaction. Per `re/NOTES.md` §C.38 a header record contains the
//! row's own QB-ID as a string field with type code `0x000E` and a
//! fixed length of `0x10` (16) base-62 characters, followed elsewhere
//! in the slot by `TxnDate` (u32 LE days since 1981-01-01) and an
//! internal counter (u32 LE).
//!
//! This scanner is deliberately conservative: it extracts only the
//! QB-ID and a candidate date/counter pair. Richer field maps (memo,
//! customer, total, etc.) are deferred (Phase 2.1 full).
//!
//! ## Anchor
//!
//! `0E 00 10 <16 base62>` - the SA17 type-tagged string header for a
//! 16-byte ASCII field.
//!
//! Header pages are selected via [`PageAttribution`] by suffix-matching
//! the owning table name against `_header`.

use opensqlany::{ApModel, PageStore};

use crate::date::{DateScan, DateSource, scan_date_counter};
use crate::decode::{DecodedPage, Directory};
use crate::error::Error;
use crate::page_attribution::PageAttribution;
use crate::scan::{ReadOptions, RecordFlag, Scan, Warning, WarningKind, for_each_extent_page};

const PAGE_DATA_END: usize = 0xFF0;
const QB_ID_LEN: usize = 16;
const HEADER_PREFIX: [u8; 3] = [0x0E, 0x00, 0x10];
const HEADER_SUFFIX: &str = "_header";

/// A minimal transaction-header record.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TransactionHeader {
    /// Row QB-ID (16 base62 chars) - primary key of the transaction.
    pub qb_id: String,
    /// Owning `SYSTABLE` name (e.g. `abmc_invoice_header`).
    pub source_table: String,
    /// SA17 transaction date (SA-day), set only when `date_source` is
    /// [`DateSource::WindowScan`].
    pub txn_date_raw: Option<u32>,
    /// Internal counter / row sequence number, set under the same condition.
    pub counter: Option<u32>,
    /// How `txn_date_raw` was found. `NotFound` and `Ambiguous` leave the
    /// date and counter unset.
    pub date_source: DateSource,
    /// Reasons this record deserves less trust. Empty when no check flagged it.
    pub flags: Vec<RecordFlag>,
    /// Source page number in the QBW file.
    pub page_number: u64,
    /// Byte offset of the QB-ID anchor within the decoded page body.
    pub page_offset: usize,
}

impl TransactionHeader {
    /// Build a header with the given ID and source table, all other fields
    /// empty. For tests and tools that need to synthesize records; scans
    /// never call it.
    #[doc(hidden)]
    pub fn synthetic(qb_id: &str, source_table: &str) -> Self {
        Self {
            qb_id: qb_id.to_owned(),
            source_table: source_table.to_owned(),
            txn_date_raw: None,
            counter: None,
            date_source: DateSource::NotFound,
            flags: Vec::new(),
            page_number: 0,
            page_offset: 0,
        }
    }

    /// The transaction date as days since the Unix epoch (1970-01-01), with
    /// the crate's SA-day offset (see `txn_date_raw` for the stored value).
    /// The offset is fitted to one sample file and not confirmed against a
    /// date known from another source; a correction ships as a bug fix.
    /// `None` when the date is missing or outside the plausible range.
    pub fn txn_date_days_since_unix(&self) -> Option<i64> {
        self.txn_date_raw.and_then(crate::date::sa_day_to_unix_day)
    }

    /// QB transaction type derived from the source table name by
    /// stripping the `abmc_` prefix and `_header` suffix.
    /// E.g. `abmc_invoice_header` -> `invoice`,
    /// `abmc_general_journal_header` -> `general_journal`.
    pub fn txn_type(&self) -> &str {
        let s = self.source_table.as_str();
        let s = s.strip_prefix("abmc_").unwrap_or(s);
        s.strip_suffix("_header").unwrap_or(s)
    }
}

/// Collect transaction headers from every `E` page that [`PageAttribution`]
/// attributes to a `*_header` table.
///
/// In strict mode the first page that fails to decode is returned as an
/// error; under [`Leniency::RECORDS`](crate::Leniency::RECORDS) it is listed
/// in [`Scan::rejected`]. Pages whose slot directory is not confirmed are
/// read with the linear scan and counted in a
/// [`WarningKind::UnconfirmedDirectoryPages`] warning.
pub(crate) fn scan_transaction_headers(
    store: &PageStore,
    model: &ApModel,
    attribution: &PageAttribution,
    options: &ReadOptions,
) -> Result<Scan<TransactionHeader>, Error> {
    let mut scan = Scan::default();
    let (mut unconfirmed, mut rows_without_id) = (0u64, 0u64);
    for_each_extent_page(
        store,
        model,
        options,
        &mut scan,
        |pn| {
            attribution
                .attribute(pn)
                .is_some_and(|e| is_header_table(&e.name))
        },
        |decoded, scan| {
            let pn = decoded.pn();
            let source_table = attribution
                .attribute(pn)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            // Prefer the slotted-page row scan when a confirmed directory
            // exists; it emits at most one record per row and keeps the
            // date search inside the row. Otherwise use the linear scan.
            let before = scan.items.len();
            match scan_page_slotted(decoded, &source_table, &mut scan.items)? {
                SlottedOutcome::Read { rows_without_id: n } => rows_without_id += n,
                outcome => {
                    unconfirmed += (outcome == SlottedOutcome::Unconfirmed) as u64;
                    scan_page(
                        &decoded.bytes()[..PAGE_DATA_END],
                        pn,
                        &source_table,
                        &mut scan.items,
                    );
                    for h in &mut scan.items[before..] {
                        let start = h.page_offset;
                        let end = (start + HEADER_PREFIX.len() + QB_ID_LEN + 96).min(PAGE_DATA_END);
                        h.flags.extend(decoded.flags_for(start..end));
                    }
                }
            }
            Ok(())
        },
    )?;
    if unconfirmed > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::UnconfirmedDirectoryPages {
                pages: unconfirmed,
            }));
    }
    if rows_without_id > 0 {
        scan.warnings
            .push(Warning::new(WarningKind::HeaderRowsWithoutId {
                rows: rows_without_id,
            }));
    }
    Ok(scan)
}

#[derive(Debug, PartialEq, Eq)]
enum SlottedOutcome {
    /// Rows were read through a confirmed directory.
    Read {
        /// Live rows that held no header ID and gave no record.
        rows_without_id: u64,
    },
    /// The page has no directory.
    Absent,
    /// The page has a directory its prelude does not confirm.
    Unconfirmed,
}

fn is_header_table(name: &str) -> bool {
    name.ends_with(HEADER_SUFFIX)
}

/// Scan a decoded page body for QB-ID header anchors and append parsed
/// records to `out`.
fn scan_page(body: &[u8], pn: u64, source_table: &str, out: &mut Vec<TransactionHeader>) {
    if body.len() < HEADER_PREFIX.len() + QB_ID_LEN {
        return;
    }
    let end = body.len().min(PAGE_DATA_END);
    if end < HEADER_PREFIX.len() + QB_ID_LEN {
        return;
    }
    let limit = end - (HEADER_PREFIX.len() + QB_ID_LEN);
    let mut pos = 0usize;
    while pos <= limit {
        if body[pos] != HEADER_PREFIX[0]
            || body[pos + 1] != HEADER_PREFIX[1]
            || body[pos + 2] != HEADER_PREFIX[2]
        {
            pos += 1;
            continue;
        }
        let id_start = pos + HEADER_PREFIX.len();
        let id_bytes = &body[id_start..id_start + QB_ID_LEN];
        if !is_base62(id_bytes) {
            pos += 1;
            continue;
        }
        let qb_id = std::str::from_utf8(id_bytes)
            .expect("guarded by is_base62")
            .to_owned();

        // Search the 96 bytes after the QB-ID for a (date, counter) pair.
        let scan = find_date_counter(body, id_start + QB_ID_LEN);

        out.push(TransactionHeader {
            qb_id,
            source_table: source_table.to_owned(),
            txn_date_raw: scan.date,
            counter: scan.counter,
            date_source: scan.source,
            flags: {
                // Without row boundaries any candidate may be a foreign key.
                let mut f = date_flags(scan.source);
                f.push(RecordFlag::HeaderIdAmbiguous);
                f
            },
            page_number: pn,
            page_offset: pos,
        });
        // Advance past the QB-ID to avoid overlapping matches.
        pos = id_start + QB_ID_LEN;
    }
}

fn date_flags(source: DateSource) -> Vec<RecordFlag> {
    if source == DateSource::Ambiguous {
        vec![RecordFlag::DateAmbiguous]
    } else {
        Vec::new()
    }
}

fn find_date_counter(body: &[u8], start: usize) -> DateScan {
    scan_date_counter(body, start, start + 96, 10_000_000)
}

/// Slotted-page-aware header scanner.
///
/// For each live slot, search the slot's byte range for the first
/// `0E 00 10 <16 base62>` anchor and emit exactly one
/// [`TransactionHeader`]. This contains the date/counter search to the
/// owning row, eliminating cross-row contamination that the linear
/// scanner can suffer.
///
/// Callers fall back to [`scan_page`] unless the outcome is
/// [`SlottedOutcome::Read`].
fn scan_page_slotted(
    decoded: &DecodedPage,
    source_table: &str,
    out: &mut Vec<TransactionHeader>,
) -> Result<SlottedOutcome, Error> {
    let pn = decoded.pn();
    let sp = match decoded.slotted()? {
        Directory::Confirmed(sp) => sp,
        Directory::Absent => return Ok(SlottedOutcome::Absent),
        Directory::Unconfirmed => return Ok(SlottedOutcome::Unconfirmed),
    };
    let mut rows_without_id = 0u64;
    for r in sp.rows() {
        let (slot_off, row) = (r.offset(), r.bytes());
        if row.len() < HEADER_PREFIX.len() + QB_ID_LEN {
            rows_without_id += 1;
            continue;
        }
        let limit = row.len() - (HEADER_PREFIX.len() + QB_ID_LEN);
        // Every `0E 00 10 <16 base62>` in the row is a candidate for the
        // row's own ID; foreign keys have the same shape. The first is
        // taken, and the row is flagged when there was more than one.
        let mut candidates = Vec::new();
        let mut i = 0usize;
        while i <= limit {
            if row[i..i + 3] == HEADER_PREFIX && is_base62(&row[i + 3..i + 3 + QB_ID_LEN]) {
                candidates.push(i);
                i += 3 + QB_ID_LEN;
            } else {
                i += 1;
            }
        }
        let Some(&first) = candidates.first() else {
            rows_without_id += 1;
            continue;
        };
        let qb_id = String::from_utf8_lossy(&row[first + 3..first + 3 + QB_ID_LEN]).into_owned();
        let scan = find_date_counter(row, first + 3 + QB_ID_LEN);
        let mut flags = date_flags(scan.source);
        if candidates.len() > 1 {
            flags.push(RecordFlag::HeaderIdAmbiguous);
        }
        let start = slot_off as usize;
        flags.extend(decoded.flags_for(start..start + row.len()));
        out.push(TransactionHeader {
            qb_id,
            source_table: source_table.to_owned(),
            txn_date_raw: scan.date,
            counter: scan.counter,
            date_source: scan.source,
            flags,
            page_number: pn,
            page_offset: slot_off as usize + first,
        });
    }
    Ok(SlottedOutcome::Read { rows_without_id })
}

fn is_base62(s: &[u8]) -> bool {
    s.iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txn_type_strips_prefix_and_suffix() {
        let h = TransactionHeader {
            qb_id: "0000000000001QBm".into(),
            source_table: "abmc_invoice_header".into(),
            txn_date_raw: None,
            counter: None,
            date_source: DateSource::NotFound,
            flags: Vec::new(),
            page_number: 0,
            page_offset: 0,
        };
        assert_eq!(h.txn_type(), "invoice");
    }

    #[test]
    fn txn_type_compound_name() {
        let h = TransactionHeader {
            qb_id: "0000000000001QBm".into(),
            source_table: "abmc_general_journal_header".into(),
            txn_date_raw: None,
            counter: None,
            date_source: DateSource::NotFound,
            flags: Vec::new(),
            page_number: 0,
            page_offset: 0,
        };
        assert_eq!(h.txn_type(), "general_journal");
    }

    #[test]
    fn is_header_table_recognises_suffix() {
        assert!(is_header_table("abmc_invoice_header"));
        assert!(is_header_table("abmc_bill_header"));
        assert!(!is_header_table("abmc_invoice_lineitem"));
        assert!(!is_header_table("SYSTABLE"));
    }

    #[test]
    fn scan_page_finds_synthetic_header() {
        // Craft a body with one valid header anchor at offset 64.
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        let anchor = 64;
        body[anchor..anchor + 3].copy_from_slice(&HEADER_PREFIX);
        body[anchor + 3..anchor + 3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBm");
        // Place a (date, counter) pair just after the QB-ID.
        let after = anchor + 3 + QB_ID_LEN;
        body[after..after + 4].copy_from_slice(&15511u32.to_le_bytes());
        body[after + 4..after + 8].copy_from_slice(&12671u32.to_le_bytes());

        let mut out = Vec::new();
        scan_page(&body, 3628, "abmc_invoice_header", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].qb_id, "0000000000001QBm");
        assert_eq!(out[0].source_table, "abmc_invoice_header");
        assert_eq!(out[0].txn_date_raw, Some(15511));
        assert_eq!(out[0].counter, Some(12671));
        assert_eq!(out[0].page_offset, anchor);
    }

    #[test]
    fn scan_page_rejects_non_base62_qbid() {
        let mut body = vec![0xAAu8; PAGE_DATA_END];
        let anchor = 64;
        body[anchor..anchor + 3].copy_from_slice(&HEADER_PREFIX);
        // Contains '!' which is not base62.
        body[anchor + 3..anchor + 3 + QB_ID_LEN].copy_from_slice(b"0000000000001QB!");
        let mut out = Vec::new();
        scan_page(&body, 3628, "abmc_invoice_header", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_page_advances_past_match() {
        // Two adjacent valid anchors back-to-back.
        let mut body = vec![0u8; PAGE_DATA_END];
        let first = 16;
        body[first..first + 3].copy_from_slice(&HEADER_PREFIX);
        body[first + 3..first + 3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBm");
        let second = first + 3 + QB_ID_LEN;
        body[second..second + 3].copy_from_slice(&HEADER_PREFIX);
        body[second + 3..second + 3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBn");
        let mut out = Vec::new();
        scan_page(&body, 3628, "abmc_invoice_header", &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].qb_id, "0000000000001QBm");
        assert_eq!(out[1].qb_id, "0000000000001QBn");
    }

    /// Helper: build a 4096-byte synthetic plaintext page with a single
    /// row whose body lives at `row_off..0xFF0` and whose slot directory
    /// is exactly one live entry at offset `dir_off` followed by enough
    /// deleted slots to satisfy `MIN_LIVE_SLOTS` (8). To do that the
    /// directory must contain at least 8 live offsets, so we lay out 8
    /// rows of varying offsets and put the anchor in the first row.
    fn build_slotted_page(rows: &[(u16, &[u8])]) -> Vec<u8> {
        let mut page = vec![0u8; 4096];
        // Prelude: the slot count as a u16 four bytes before the array,
        // confirmed by the trailer's count byte at 0xFF0.
        page[0xFC..0xFE].copy_from_slice(&(rows.len() as u16).to_le_bytes());
        page[PAGE_DATA_END] = rows.len() as u8;
        // Write each row body at its offset.
        for &(off, body) in rows {
            let start = off as usize;
            page[start..start + body.len()].copy_from_slice(body);
        }
        // Slot directory: descending offsets, little-endian u16, at
        // offset 0x100 (well within SEARCH_LIMIT and after row data).
        // Sort offsets descending.
        let mut offs: Vec<u16> = rows.iter().map(|(o, _)| *o).collect();
        offs.sort_by(|a, b| b.cmp(a));
        let mut dir = 0x100usize;
        for o in &offs {
            page[dir..dir + 2].copy_from_slice(&o.to_le_bytes());
            dir += 2;
        }
        page
    }

    fn slotted(page: Vec<u8>, pn: u64, out: &mut Vec<TransactionHeader>) -> SlottedOutcome {
        let decoded = DecodedPage::from_plain(pn, page);
        scan_page_slotted(&decoded, "abmc_invoice_header", out).unwrap()
    }

    #[test]
    fn unconfirmed_directory_falls_back_to_the_linear_scan() {
        // Same layout without the prelude count: opensqlany cannot confirm
        // the directory, so the slotted path declines.
        let bodies: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 64]).collect();
        let rows: Vec<(u16, &[u8])> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| (0x800 + (i as u16) * 64, b.as_slice()))
            .collect();
        let mut page = build_slotted_page(&rows);
        page[0xFC] = 0;
        let mut out = Vec::new();
        assert_eq!(slotted(page, 3, &mut out), SlottedOutcome::Unconfirmed);
    }

    #[test]
    fn scan_page_slotted_emits_one_record_per_row() {
        // Build 8 rows (min live slots), each carrying a distinct
        // header anchor.
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        let qb_ids: Vec<String> = (0..8)
            .map(|i| format!("00000000000{:05}", i + 1000))
            .collect();
        for id in &qb_ids {
            let mut body = vec![0u8; 64];
            body[0..3].copy_from_slice(&HEADER_PREFIX);
            body[3..3 + QB_ID_LEN].copy_from_slice(id.as_bytes());
            body[19..23].copy_from_slice(&15511u32.to_le_bytes());
            body[23..27].copy_from_slice(&12671u32.to_le_bytes());
            bodies.push(body);
        }
        // Place row bodies at descending offsets that exceed 0x300
        // (out of the slot-directory search window).
        let offsets: Vec<u16> = (0..8).map(|i| 0x800 + (i as u16) * 64).collect();
        let rows: Vec<(u16, &[u8])> = offsets
            .iter()
            .zip(bodies.iter())
            .map(|(o, b)| (*o, b.as_slice()))
            .collect();
        let page = build_slotted_page(&rows);
        let mut out = Vec::new();
        assert_eq!(
            slotted(page, 4242, &mut out),
            SlottedOutcome::Read { rows_without_id: 0 }
        );
        assert_eq!(out.len(), 8);
        // Every emitted record should reference the correct page.
        for h in &out {
            assert_eq!(h.page_number, 4242);
            assert_eq!(h.source_table, "abmc_invoice_header");
            assert_eq!(h.txn_date_raw, Some(15511));
            assert_eq!(h.counter, Some(12671));
        }
        // The set of QB-IDs should match what we wrote.
        let mut got: Vec<String> = out.iter().map(|h| h.qb_id.clone()).collect();
        got.sort();
        let mut want = qb_ids.clone();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn row_with_two_id_shaped_strings_is_flagged() {
        // Row 0 holds its own ID and then a foreign key in the same shape.
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        for i in 0..8 {
            let mut body = vec![0u8; 96];
            body[0..3].copy_from_slice(&HEADER_PREFIX);
            body[3..19].copy_from_slice(format!("00000000000{:05}", i + 1000).as_bytes());
            if i == 0 {
                body[40..43].copy_from_slice(&HEADER_PREFIX);
                body[43..59].copy_from_slice(b"FOREIGNKEYFOREIG");
            }
            bodies.push(body);
        }
        let rows: Vec<(u16, &[u8])> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| (0x800 + (i as u16) * 96, b.as_slice()))
            .collect();
        let page = build_slotted_page(&rows);
        let mut out = Vec::new();
        assert_eq!(
            slotted(page, 1, &mut out),
            SlottedOutcome::Read { rows_without_id: 0 }
        );
        assert_eq!(out.len(), 8);
        let flagged: Vec<_> = out
            .iter()
            .filter(|h| h.flags.contains(&RecordFlag::HeaderIdAmbiguous))
            .collect();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].qb_id, "0000000000001000");
    }

    #[test]
    fn scan_page_slotted_returns_none_without_directory() {
        // All-zero page has no slot directory.
        let page = vec![0u8; 4096];
        let mut out = Vec::new();
        assert_eq!(slotted(page, 1, &mut out), SlottedOutcome::Absent);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_page_slotted_skips_rows_without_anchor() {
        // 8 rows, only the first carries a header anchor; others are
        // free-form bytes that contain no `0E 00 10 <base62*16>` match.
        let mut bodies: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 64]).collect();
        // Row 0: real header anchor.
        bodies[0] = {
            let mut body = vec![0u8; 64];
            body[0..3].copy_from_slice(&HEADER_PREFIX);
            body[3..3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBm");
            body
        };
        let offsets: Vec<u16> = (0..8).map(|i| 0x800 + (i as u16) * 64).collect();
        let rows: Vec<(u16, &[u8])> = offsets
            .iter()
            .zip(bodies.iter())
            .map(|(o, b)| (*o, b.as_slice()))
            .collect();
        let page = build_slotted_page(&rows);
        let mut out = Vec::new();
        // S2-21: the seven rows without an ID are counted, not just skipped.
        assert_eq!(
            slotted(page, 7, &mut out),
            SlottedOutcome::Read { rows_without_id: 7 }
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].qb_id, "0000000000001QBm");
    }

    #[test]
    fn scan_page_slotted_emits_at_most_one_per_slot() {
        // Build 8 rows where row 0 contains TWO back-to-back header
        // anchors. Slotted scan must emit only the first.
        let mut bodies: Vec<Vec<u8>> = (1..8).map(|i| vec![i as u8; 64]).collect();
        let mut row0 = vec![0u8; 128];
        row0[0..3].copy_from_slice(&HEADER_PREFIX);
        row0[3..3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBm");
        let second = 3 + QB_ID_LEN;
        row0[second..second + 3].copy_from_slice(&HEADER_PREFIX);
        row0[second + 3..second + 3 + QB_ID_LEN].copy_from_slice(b"0000000000001QBn");
        bodies.insert(0, row0);
        let offsets: Vec<u16> = (0..8).map(|i| 0x700 + (i as u16) * 128).collect();
        let rows: Vec<(u16, &[u8])> = offsets
            .iter()
            .zip(bodies.iter())
            .map(|(o, b)| (*o, b.as_slice()))
            .collect();
        let page = build_slotted_page(&rows);
        let mut out = Vec::new();
        assert_eq!(
            slotted(page, 9, &mut out),
            SlottedOutcome::Read { rows_without_id: 7 }
        );
        // The only "1QBm"/"1QBn" record we expect is the FIRST one.
        let qbm = out.iter().filter(|h| h.qb_id == "0000000000001QBm").count();
        let qbn = out.iter().filter(|h| h.qb_id == "0000000000001QBn").count();
        assert_eq!(qbm, 1);
        assert_eq!(qbn, 0);
    }
}
