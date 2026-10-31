//! A synthetic QuickBooks company file for integration tests.
//!
//! Built from the layouts documented in `SPECIFICATION.md` and the unit
//! tests; no real company file is involved. Page map:
//!
//! | page | type | content |
//! |------|------|---------|
//! | 0 | superblock | magic, version (3, 201, 12), `flags_06` |
//! | 1 | `@` | AP fill only, calibrates the keystream |
//! | 2 | `E` | `SYSTABLE` rows for the three tables below |
//! | 3 | `E` | empty |
//! | 4 | `E` | `abmc_invoice_header`: one header |
//! | 5 | `E` | `abmc_invoice_inventory_lineitem`: two invoice lines |
//! | 6 | `E` | `abmc_general_journal_inventory_lineitem`: a balanced pair |
//! | 7 | `E` | same table; with `bad_record`, one anchor with amount type 0x5A |

#![allow(dead_code)] // each test binary uses a different subset

pub const PAGE: usize = 4096;
const DATA_END: usize = 0xFF0;
const BV: u8 = 0x5A;

pub const HEADER_ID: &str = "0000000000001QBm";
pub const JOURNAL_ID: &str = "0000000000002JRn";
/// SA-day of every record (unix day 19800, 2024-03-18, under the crate's
/// offset).
pub const SA_DAY: u32 = 23_817;

/// The fixture as bytes. With `bad_record`, page 7 holds a line-item anchor
/// followed by an unknown amount-type byte.
pub fn synthetic_qbw(bad_record: bool) -> Vec<u8> {
    let mut pages: Vec<Vec<u8>> = Vec::new();
    pages.push(superblock());
    pages.push(encode(&[0u8; PAGE], 1, b'@', [3, 5, 7, 9, 11, 13, 15, 17]));

    let mut systable = vec![0u8; PAGE];
    put_systable_row(&mut systable, 0x100, 100, "abmc_invoice_header", 4, 4);
    put_systable_row(
        &mut systable,
        0x200,
        101,
        "abmc_invoice_inventory_lineitem",
        5,
        5,
    );
    put_systable_row(
        &mut systable,
        0x300,
        102,
        "abmc_general_journal_inventory_lineitem",
        6,
        7,
    );
    pages.push(e_page(systable, 2));
    pages.push(e_page(vec![0u8; PAGE], 3));

    let mut header = vec![0u8; PAGE];
    put_header(&mut header, 0x400, HEADER_ID, 41);
    pages.push(e_page(header, 4));

    let mut invoice = vec![0u8; PAGE];
    put_lineitem(
        &mut invoice,
        0x400,
        HEADER_ID,
        b"ITEM000000000001",
        [0x02, 0x40, 0x0F, 0x04],
    );
    put_lineitem(
        &mut invoice,
        0x600,
        HEADER_ID,
        b"ITEM000000000002",
        [0x02, 0x80, 0x0F, 0x04],
    );
    pages.push(e_page(invoice, 5));

    let mut journal = vec![0u8; PAGE];
    put_lineitem(
        &mut journal,
        0x400,
        JOURNAL_ID,
        b"ITEM000000000003",
        [0x02, 0x40, 0x2D, 0x01],
    );
    put_lineitem(
        &mut journal,
        0x600,
        JOURNAL_ID,
        b"ITEM000000000004",
        [0x02, 0xC0, 0x2D, 0x01],
    );
    pages.push(e_page(journal, 6));

    let mut tail = vec![0u8; PAGE];
    if bad_record {
        put_lineitem(
            &mut tail,
            0x400,
            JOURNAL_ID,
            b"ITEM000000000005",
            [0x5A, 0x01, 0x02, 0x03],
        );
    }
    pages.push(e_page(tail, 7));

    pages.concat()
}

fn superblock() -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    p[0x06] = 0x09;
    p[0x10..0x14].copy_from_slice(&3u32.to_le_bytes());
    p[0x14..0x18].copy_from_slice(&0xDA7A_BA5Eu32.to_le_bytes());
    p[0x18..0x1A].copy_from_slice(&201u16.to_le_bytes());
    p[0x1A..0x1C].copy_from_slice(&12u16.to_le_bytes());
    seal(&mut p);
    p
}

/// An `E` page carrying the QuickBooks C.36 anchor `[rc] 00 D5 0B` (with a
/// trailer row count of 0) so its keystream is confirmed.
fn e_page(mut plain: Vec<u8>, pn: u64) -> Vec<u8> {
    plain[0xF00..0xF04].copy_from_slice(&[0x00, 0x00, 0xD5, 0x0B]);
    encode(&plain, pn, b'E', [0; 8])
}

/// AP-encode `plain` as page `pn` of type `ty` with per-sector `steps`,
/// then write the plaintext trailer and CRC.
fn encode(plain: &[u8], pn: u64, ty: u8, steps: [u8; 8]) -> Vec<u8> {
    let bias = ((pn % 16) as u8) / 2 * 4;
    let mut raw = plain.to_vec();
    for (i, b) in raw[..DATA_END].iter_mut().enumerate() {
        let si = i / 512;
        let base = BV
            .wrapping_add(pn as u8)
            .wrapping_add(si as u8)
            .wrapping_sub(bias);
        let idx = (i % 512) as u8;
        *b = b
            .wrapping_add(base)
            .wrapping_add(idx.wrapping_mul(steps[si]));
    }
    raw[DATA_END..].fill(0);
    raw[0xFF2] = ty;
    raw[0xFF4..0xFF8].copy_from_slice(&(pn as u32 + 1).to_le_bytes());
    seal(&mut raw);
    raw
}

fn seal(p: &mut [u8]) {
    let crc = crc32fast::hash(&p[..0xFFC]);
    p[0xFFC..].copy_from_slice(&crc.to_le_bytes());
}

fn put_systable_row(p: &mut [u8], at: usize, tid: u32, name: &str, root: u32, last: u32) {
    p[at] = 0x05;
    p[at + 4..at + 8].copy_from_slice(&tid.to_le_bytes());
    p[at + 12..at + 16].copy_from_slice(&[0xB1, 0x0D, 0x19, 0x0D]);
    p[at + 20] = name.len() as u8;
    let name_end = at + 21 + name.len();
    p[at + 21..name_end].copy_from_slice(name.as_bytes());
    p[name_end + 6] = 12; // col_count
    p[name_end + 34..name_end + 38].copy_from_slice(&root.to_le_bytes());
    p[name_end + 50..name_end + 54].copy_from_slice(&last.to_le_bytes());
}

fn put_header(p: &mut [u8], at: usize, id: &str, counter: u32) {
    p[at..at + 3].copy_from_slice(&[0x0E, 0x00, 0x10]);
    p[at + 3..at + 19].copy_from_slice(id.as_bytes());
    p[at + 19..at + 23].copy_from_slice(&SA_DAY.to_le_bytes());
    p[at + 23..at + 27].copy_from_slice(&counter.to_le_bytes());
}

fn put_lineitem(p: &mut [u8], at: usize, parent: &str, item: &[u8; 16], amount: [u8; 4]) {
    p[at - 19..at - 16].copy_from_slice(&[0x04, 0x00, 0x10]);
    p[at - 16..at].copy_from_slice(item);
    p[at + 4] = 0x10;
    p[at + 5..at + 21].copy_from_slice(parent.as_bytes());
    p[at + 21..at + 25].copy_from_slice(&[0x00, 0x00, 0x80, 0x3F]);
    p[at + 25..at + 29].copy_from_slice(&amount);
    p[at + 33..at + 37].copy_from_slice(&SA_DAY.to_le_bytes());
    p[at + 37..at + 41].copy_from_slice(&7u32.to_le_bytes());
}
