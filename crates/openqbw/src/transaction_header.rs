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
//! `0E 00 10 <16 base62>` — the SA17 type-tagged string header for a
//! 16-byte ASCII field.
//!
//! Header pages are selected via [`PageAttribution`] by suffix-matching
//! the owning table name against `_header`.

use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType, Result as SaResult};

use crate::bv_recovery::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};
use crate::page_attribution::PageAttribution;

const PAGE_DATA_END: usize = 0xFF0;
const QB_ID_LEN: usize = 16;
const HEADER_PREFIX: [u8; 3] = [0x0E, 0x00, 0x10];
const HEADER_SUFFIX: &str = "_header";

/// A minimal transaction-header record.
#[derive(Debug, Clone)]
pub struct TransactionHeader {
    /// Row QB-ID (16 base62 chars) — primary key of the transaction.
    pub qb_id: String,
    /// Owning `SYSTABLE` name (e.g. `abmc_invoice_header`).
    pub source_table: String,
    /// SA17 transaction date (days since 1981-01-01), when discovered.
    pub txn_date_raw: Option<u32>,
    /// Internal counter / row sequence number, when discovered.
    pub counter: Option<u32>,
    /// Source page number in the QBW file.
    pub page_number: u64,
    /// Byte offset of the QB-ID anchor within the decoded page body.
    pub page_offset: usize,
}

impl TransactionHeader {
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

/// Iterate transaction headers across every E-page that
/// [`PageAttribution`] attributes to a `*_header` table.
pub fn iter_transaction_headers<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: &'a PageAttribution,
) -> impl Iterator<Item = TransactionHeader> + 'a {
    TransactionHeaderIter::new(store, model, attribution)
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

        // Search a 96-byte window before and after the QB-ID for a
        // plausible (date, counter) pair. SA dates fall in 13000..20000
        // (roughly 2016..2034). The internal counter is non-zero and
        // bounded by 32 bits worth of plausible row indices.
        let (txn_date_raw, counter) = find_date_counter(body, id_start + QB_ID_LEN);

        out.push(TransactionHeader {
            qb_id,
            source_table: source_table.to_owned(),
            txn_date_raw,
            counter,
            page_number: pn,
            page_offset: pos,
        });
        // Advance past the QB-ID to avoid overlapping matches.
        pos = id_start + QB_ID_LEN;
    }
}

fn find_date_counter(body: &[u8], start: usize) -> (Option<u32>, Option<u32>) {
    let end = (start + 96).min(body.len().saturating_sub(8));
    let mut i = start;
    while i + 8 <= end {
        let d = u32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
        let c = u32::from_le_bytes([body[i + 4], body[i + 5], body[i + 6], body[i + 7]]);
        if (13_000..20_000).contains(&d) && c > 0 && c < 10_000_000 {
            return (Some(d), Some(c));
        }
        i += 1;
    }
    (None, None)
}

fn is_base62(s: &[u8]) -> bool {
    s.iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

struct TransactionHeaderIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    attribution: &'a PageAttribution,
    pn: u64,
    n_pages: u64,
    buffer: Vec<TransactionHeader>,
}

impl<'a> TransactionHeaderIter<'a> {
    fn new(store: &'a PageStore, model: &'a ApModel, attribution: &'a PageAttribution) -> Self {
        Self {
            store,
            model,
            attribution,
            pn: 1,
            n_pages: store.page_count(),
            buffer: Vec::new(),
        }
    }

    fn fill_buffer(&mut self) -> SaResult<bool> {
        while self.buffer.is_empty() && self.pn < self.n_pages {
            let pn = self.pn;
            self.pn += 1;

            // Skip pages that the attribution map does not assign to a
            // header table.
            let source_table = match self.attribution.attribute(pn) {
                Some(entry) if is_header_table(&entry.name) => entry.name.clone(),
                _ => continue,
            };

            let page = self.store.page(pn)?;
            if page.trailer().page_type() != PageType::Extent {
                continue;
            }
            let raw = page.bytes();
            // Header pages may or may not carry the QB-data anchor.
            // Try the QB-specific recovery first, fall back to the
            // E-page oracle (plain[0] == 0), and finally to the generic
            // AP model.
            let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
                deobfuscate_with_bv(raw, pn, bv)
            } else {
                let bv = oracle_bv_e_page(pn, raw);
                let candidate = deobfuscate_with_bv(raw, pn, bv);
                if candidate[0] == 0 {
                    candidate
                } else {
                    self.model.deobfuscate_with_store(raw, pn, self.store)
                }
            };
            let before = self.buffer.len();
            scan_page(&plain[..PAGE_DATA_END], pn, &source_table, &mut self.buffer);
            // Reverse for LIFO consumption to preserve source order.
            self.buffer[before..].reverse();
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for TransactionHeaderIter<'_> {
    type Item = TransactionHeader;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(h) = self.buffer.pop() {
                return Some(h);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                _ => return None,
            }
        }
    }
}

impl FusedIterator for TransactionHeaderIter<'_> {}

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
        body[anchor + 3..anchor + 3 + QB_ID_LEN]
            .copy_from_slice(b"0000000000001QBm");
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
        body[anchor + 3..anchor + 3 + QB_ID_LEN]
            .copy_from_slice(b"0000000000001QB!");
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
        body[first + 3..first + 3 + QB_ID_LEN]
            .copy_from_slice(b"0000000000001QBm");
        let second = first + 3 + QB_ID_LEN;
        body[second..second + 3].copy_from_slice(&HEADER_PREFIX);
        body[second + 3..second + 3 + QB_ID_LEN]
            .copy_from_slice(b"0000000000001QBn");
        let mut out = Vec::new();
        scan_page(&body, 3628, "abmc_invoice_header", &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].qb_id, "0000000000001QBm");
        assert_eq!(out[1].qb_id, "0000000000001QBn");
    }
}
