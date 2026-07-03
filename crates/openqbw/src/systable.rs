//! SYSTABLE catalog row parser (Phase 4.3).
//!
//! The SA17 `SYSTABLE` system catalog stores one row per table in the
//! database. On Rock Castle Construction and related QBW corpora, each row
//! is anchored by a fixed 16-byte tag pattern:
//!
//! ```text
//! 05 00 00 00 <tid u32 LE> 00 00 00 00 <magic 4B> 00 00 00 00 <name_len u8> <name>
//! ```
//!
//! The 4-byte `<magic>` field is FILE-SPECIFIC (it appears to be a per-database
//! salt). See `re/NOTES.md` §C.18 and §C.20. We therefore accept any 4 bytes
//! there and rely on the surrounding zero markers, the name-length sanity
//! check, and the ASCII validity of the table name to disambiguate.
//!
//! Beyond the row tag, the trailer that follows the name encodes per-table
//! metadata; see §C.33. We extract three fields:
//!
//! * `col_count` at trailer offset +6  (1 byte)
//! * `data_root_page` at +34  (u32 LE) - first/leftmost leaf of the table B-tree
//! * `last_page` at +50  (u32 LE) - rightmost leaf
//!
//! Rows are scanned across every decoded `E`-type page of the QBW file. The
//! same QB-specific AP-cipher recovery is used as for line-item extraction
//! (`recover_bv_qb_data` with fallback to the generic `ApModel`).

use std::collections::BTreeMap;
use std::iter::FusedIterator;

use opensqlany::{ApModel, PageStore, PageType, Result as SaResult};

use crate::bv_recovery::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};

const PAGE_DATA_END: usize = 0xFF0;
const NAME_LEN_MIN: u8 = 4;
const NAME_LEN_MAX: u8 = 64;
const TRAILER_SCAN_LEN: usize = 64;
const COL_COUNT_OFF: usize = 6;
const DATA_ROOT_OFF: usize = 34;
const LAST_PAGE_OFF: usize = 50;

/// A single parsed `SYSTABLE` row.
#[derive(Debug, Clone)]
pub struct SysTableEntry {
    /// Table id (SA internal `table_id`).
    pub table_id: u32,
    /// Table name (ASCII).
    pub name: String,
    /// File-specific 4-byte magic tag observed between the table id and the
    /// name. Captured for diagnostics.
    pub magic: [u8; 4],
    /// Number of columns in the table, from trailer offset +6.
    pub col_count: Option<u8>,
    /// First (leftmost) leaf page of the table B-tree, from trailer +34.
    pub data_root_page: Option<u32>,
    /// Last (rightmost) leaf page of the table B-tree, from trailer +50.
    pub last_page: Option<u32>,
    /// Page on which this row was found.
    pub page_number: u64,
    /// Byte offset of the row tag within the decoded page body.
    pub row_offset: usize,
}

/// Scan a decoded page body for `SYSTABLE` row tags and append parsed
/// entries to `out`. Matches every occurrence of the 16-byte framed tag
/// followed by a plausible name-length byte; the file-specific 4-byte
/// magic is accepted as wildcard.
pub fn scan_page(body: &[u8], pn: u64, out: &mut Vec<SysTableEntry>) {
    if body.len() < 17 {
        return;
    }
    let end = body.len().min(PAGE_DATA_END);
    if end < 17 {
        return;
    }
    let limit = end - 17;
    let mut pos = 0usize;
    while pos <= limit {
        // Match prefix: 05 00 00 00 ?? ?? ?? ?? 00 00 00 00
        if body[pos] != 0x05
            || body[pos + 1] != 0x00
            || body[pos + 2] != 0x00
            || body[pos + 3] != 0x00
            || body[pos + 8] != 0x00
            || body[pos + 9] != 0x00
            || body[pos + 10] != 0x00
            || body[pos + 11] != 0x00
        {
            pos += 1;
            continue;
        }
        // Match suffix: <magic 4B> 00 00 00 00 <name_len>
        if body[pos + 16] != 0x00
            || body[pos + 17] != 0x00
            || body[pos + 18] != 0x00
            || body[pos + 19] != 0x00
        {
            pos += 1;
            continue;
        }
        if pos + 21 > end {
            break;
        }
        let name_len = body[pos + 20];
        if !(NAME_LEN_MIN..=NAME_LEN_MAX).contains(&name_len) {
            pos += 1;
            continue;
        }
        let name_start = pos + 21;
        let name_end = name_start + name_len as usize;
        if name_end > end {
            pos += 1;
            continue;
        }
        let name_bytes = &body[name_start..name_end];
        if !name_bytes.iter().all(|&b| (32..127).contains(&b)) {
            pos += 1;
            continue;
        }

        let tid = u32::from_le_bytes([body[pos + 4], body[pos + 5], body[pos + 6], body[pos + 7]]);
        let magic = [
            body[pos + 12],
            body[pos + 13],
            body[pos + 14],
            body[pos + 15],
        ];
        let name = std::str::from_utf8(name_bytes)
            .expect("name guarded by printable-ASCII check")
            .to_owned();

        // Parse trailer fields if they fit in the page body.
        let trailer_start = name_end;
        let trailer = if trailer_start + TRAILER_SCAN_LEN <= body.len() {
            &body[trailer_start..trailer_start + TRAILER_SCAN_LEN]
        } else {
            &body[trailer_start..body.len().min(trailer_start + TRAILER_SCAN_LEN)]
        };
        let col_count = trailer.get(COL_COUNT_OFF).copied();
        let data_root_page = read_u32_le(trailer, DATA_ROOT_OFF);
        let last_page = read_u32_le(trailer, LAST_PAGE_OFF);

        out.push(SysTableEntry {
            table_id: tid,
            name,
            magic,
            col_count,
            data_root_page,
            last_page,
            page_number: pn,
            row_offset: pos,
        });
        pos = name_end;
    }
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    if buf.len() < off + 4 {
        return None;
    }
    Some(u32::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
    ]))
}

/// Iterate every `SYSTABLE` row recovered from `store`.
///
/// Pages whose `bv` cannot be recovered are silently skipped. Rows are
/// yielded in the order they are encountered; callers wanting a canonical
/// catalog should call [`collect_unique`] instead.
pub fn iter_systable_entries<'a>(
    store: &'a PageStore,
    model: &'a ApModel,
) -> impl Iterator<Item = SysTableEntry> + 'a {
    SysTableIter::new(store, model)
}

/// Collect a deduplicated catalog keyed by `(table_id, name)`, choosing
/// the first occurrence found.
pub fn collect_unique(store: &PageStore, model: &ApModel) -> Vec<SysTableEntry> {
    let mut uniq: BTreeMap<(u32, String), SysTableEntry> = BTreeMap::new();
    for entry in iter_systable_entries(store, model) {
        uniq.entry((entry.table_id, entry.name.clone()))
            .or_insert(entry);
    }
    uniq.into_values().collect()
}

struct SysTableIter<'a> {
    store: &'a PageStore,
    model: &'a ApModel,
    pn: u64,
    n_pages: u64,
    buffer: Vec<SysTableEntry>,
}

impl<'a> SysTableIter<'a> {
    fn new(store: &'a PageStore, model: &'a ApModel) -> Self {
        Self {
            store,
            model,
            pn: 1,
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
            // For SYSTABLE catalog pages the QB-data anchor is typically
            // absent; fall back to the E-page oracle (plain[0]=0x00). Final
            // fallback is the generic AP model (which may be off by 1).
            let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
                deobfuscate_with_bv(raw, pn, bv)
            } else {
                let bv = oracle_bv_e_page(pn, raw);
                let candidate = deobfuscate_with_bv(raw, pn, bv);
                // Sanity: SA17 E-pages begin with a null byte. If the oracle
                // decoded plain[0] is non-zero, the page is unusual; fall
                // through to the generic model.
                if candidate[0] == 0 {
                    candidate
                } else {
                    self.model.deobfuscate_with_store(raw, pn, self.store)
                }
            };
            let mut found = Vec::new();
            scan_page(&plain, pn, &mut found);
            // Reverse so we pop in source order.
            for entry in found.into_iter().rev() {
                self.buffer.push(entry);
            }
        }
        Ok(!self.buffer.is_empty())
    }
}

impl Iterator for SysTableIter<'_> {
    type Item = SysTableEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.buffer.pop() {
                return Some(entry);
            }
            match self.fill_buffer() {
                Ok(true) => continue,
                _ => return None,
            }
        }
    }
}

impl FusedIterator for SysTableIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal SYSTABLE row: tag + name. No trailing metadata.
    fn synth_row(tid: u32, magic: [u8; 4], name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x05, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&tid.to_le_bytes());
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&magic);
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out
    }

    #[test]
    fn scan_finds_single_row() {
        let mut body = vec![0u8; 0x100];
        let row = synth_row(5887, [0xb1, 0x0d, 0x19, 0x0d], "abmc_invoice_header");
        body[0x20..0x20 + row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 42, &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.table_id, 5887);
        assert_eq!(e.name, "abmc_invoice_header");
        assert_eq!(e.magic, [0xb1, 0x0d, 0x19, 0x0d]);
        assert_eq!(e.page_number, 42);
        assert_eq!(e.row_offset, 0x20);
    }

    #[test]
    fn scan_finds_multiple_rows_with_different_magic() {
        let mut body = vec![0u8; 0x400];
        let r1 = synth_row(100, [0xb1, 0x0d, 0x19, 0x0d], "alpha_table");
        let r2 = synth_row(200, [0x59, 0x2a, 0x16, 0x0d], "beta_table");
        body[0x20..0x20 + r1.len()].copy_from_slice(&r1);
        let r2_off = 0x20 + r1.len() + 16;
        body[r2_off..r2_off + r2.len()].copy_from_slice(&r2);
        let mut out = Vec::new();
        scan_page(&body, 1, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "alpha_table");
        assert_eq!(out[1].name, "beta_table");
        assert_eq!(out[1].magic, [0x59, 0x2a, 0x16, 0x0d]);
    }

    #[test]
    fn scan_rejects_implausible_name_len() {
        let mut body = vec![0u8; 0x100];
        let mut row = synth_row(1, [0; 4], "abcd");
        // Override name_len to 0.
        row[20] = 0;
        body[0x20..0x20 + row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert!(out.is_empty());

        let mut row = synth_row(1, [0; 4], "abcd");
        row[20] = 100; // > NAME_LEN_MAX (64)
        body[0x40..0x40 + row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        // No row matched.
        assert!(out.is_empty());
    }

    #[test]
    fn scan_rejects_non_ascii_name() {
        let mut body = vec![0u8; 0x100];
        let mut row = synth_row(7, [0; 4], "abcd");
        // Replace 'a' with 0xff (non-printable).
        let name_off = 21;
        row[name_off] = 0xff;
        body[0x10..0x10 + row.len()].copy_from_slice(&row);
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_extracts_trailer_fields() {
        let mut body = vec![0u8; 0x200];
        let row = synth_row(5887, [0xb1, 0x0d, 0x19, 0x0d], "abmc_invoice_header");
        let row_off = 0x20;
        body[row_off..row_off + row.len()].copy_from_slice(&row);

        let trailer_start = row_off + row.len();
        // Put col_count = 20 at +6
        body[trailer_start + COL_COUNT_OFF] = 20;
        // Put data_root = 3628 at +34
        body[trailer_start + DATA_ROOT_OFF..trailer_start + DATA_ROOT_OFF + 4]
            .copy_from_slice(&3628u32.to_le_bytes());
        // Put last_page = 3072 at +50
        body[trailer_start + LAST_PAGE_OFF..trailer_start + LAST_PAGE_OFF + 4]
            .copy_from_slice(&3072u32.to_le_bytes());

        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.col_count, Some(20));
        assert_eq!(e.data_root_page, Some(3628));
        assert_eq!(e.last_page, Some(3072));
    }

    #[test]
    fn scan_skips_trailer_region() {
        // A row whose tag falls inside the trailer (>= 0xFF0) must not match.
        let mut body = vec![0u8; 0x1000];
        let row = synth_row(99, [0; 4], "trail_table");
        // Place row tag at 0xFF8 (well inside the 12-byte trailer).
        let row_off = 0xFF8;
        if row_off + row.len() <= body.len() {
            body[row_off..row_off + row.len()].copy_from_slice(&row);
        }
        let mut out = Vec::new();
        scan_page(&body, 0, &mut out);
        assert!(out.is_empty());
    }
}
