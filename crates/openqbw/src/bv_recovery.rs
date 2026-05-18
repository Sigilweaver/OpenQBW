//! QuickBooks-specific bv recovery for E-pages using the C.36 anchor.
//!
//! Strategy: every E-page produced by QB has, near its slot directory, the
//! 4-byte pattern `[trailer_RC] 0x00 0xD5 0x0B` in plaintext. Since the AP
//! step is independent of `bv`, we can precompute a `C[i]` table per sector
//! (using the oracle base to recover step) and then test every bv 0..=255 by
//! subtracting it from C and looking for the anchor.
//!
//! See `OpenQBW/re/NOTES.md §C.36` for the empirical derivation.

const PAGE: usize = 4096;
const SECTOR: usize = 512;
const SECTORS: usize = 8;
const TRAILER_START: usize = 0xFF0;

const D5: u8 = 0xD5;
const ZB: u8 = 0x0B;

/// Try to recover the correct bv for E-page `pn` (zero-based) using the QB
/// page-trailer anchor. Returns `Some(bv)` on success, `None` if no candidate
/// bv decodes the anchor (page is not a QB user-data page).
///
/// `raw_page` must be exactly 4096 bytes (the full physical page including
/// the trailer at `0xFF0`).
pub fn recover_bv_qb_data(pn: u64, raw_page: &[u8]) -> Option<u8> {
    if raw_page.len() != PAGE {
        return None;
    }
    let trailer_rc = raw_page[TRAILER_START];

    let p16 = (pn % 16) as u8;
    let bias = p16 / 2 * 4;

    // Oracle bv assumes plain[0] == 0x00 (typical for SA17 page headers).
    let oracle_bv = raw_page[0]
        .wrapping_sub(pn as u8)
        .wrapping_add(bias);

    // Precompute C[si][i] using oracle-derived step per sector.
    let mut c_table: [Vec<u8>; SECTORS] = Default::default();
    for si in 0..SECTORS {
        let off = si * SECTOR;
        let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
        let sec = &raw_page[off..end];

        let oracle_base = oracle_bv
            .wrapping_add(pn as u8)
            .wrapping_add(si as u8)
            .wrapping_sub(bias);
        let step = recover_step(sec, oracle_base);

        // offset_no_bv = (pn + si - bias) mod 256
        let offset_no_bv = (pn as u8).wrapping_add(si as u8).wrapping_sub(bias);
        let mut c = vec![0u8; sec.len()];
        for (i, &b) in sec.iter().enumerate() {
            c[i] = b
                .wrapping_sub(offset_no_bv)
                .wrapping_sub((i as u8).wrapping_mul(step));
        }
        c_table[si] = c;
    }

    // Try oracle first.
    if anchor_present_with_bv(&c_table, oracle_bv, trailer_rc) {
        return Some(oracle_bv);
    }
    for bv in 0u16..=255 {
        let bv = bv as u8;
        if bv == oracle_bv {
            continue;
        }
        if anchor_present_with_bv(&c_table, bv, trailer_rc) {
            return Some(bv);
        }
    }
    None
}

/// For a candidate bv, scan the decoded page body for the anchor pattern.
///
/// The anchor `[trailer_RC, 0x00, 0xD5, 0x0B]` is searched across sector
/// boundaries by stitching adjacent decoded sectors via wrapping subtraction.
fn anchor_present_with_bv(c_table: &[Vec<u8>; SECTORS], bv: u8, rc: u8) -> bool {
    // Decode each sector into a fresh buffer. We need at most a 3-byte tail of
    // sector N to combine with the head of sector N+1 for cross-boundary
    // matches; for simplicity we decode all sectors and then scan a single
    // contiguous buffer of length TRAILER_START.
    let mut plain = [0u8; TRAILER_START];
    let mut cursor = 0usize;
    for si in 0..SECTORS {
        let c = &c_table[si];
        for (i, &cb) in c.iter().enumerate() {
            plain[cursor + i] = cb.wrapping_sub(bv);
        }
        cursor += c.len();
    }
    // Linear search for [rc, 0x00, 0xD5, 0x0B].
    if plain.len() < 4 {
        return false;
    }
    let limit = plain.len() - 4;
    let mut i = 0;
    while i <= limit {
        if plain[i] == rc
            && plain[i + 1] == 0
            && plain[i + 2] == D5
            && plain[i + 3] == ZB
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Recover the AP step for a sector using a peak-histogram approach.
///
/// For each candidate step in `0..=255`, compute `plain[i] = sec[i] - base - i*step`
/// and find the most common byte. The step with the highest peak wins. With
/// `base` set to the oracle base, the correct step maximises the count of
/// plaintext zeros, which dominates QB data sectors.
fn recover_step(sec: &[u8], base: u8) -> u8 {
    let mut best_step = 0u8;
    let mut best_peak = 0u16;
    for step in 0u16..=255 {
        let step = step as u8;
        let mut hist = [0u16; 256];
        for (i, &b) in sec.iter().enumerate() {
            let plain = b
                .wrapping_sub(base)
                .wrapping_sub((i as u8).wrapping_mul(step));
            hist[plain as usize] += 1;
        }
        let peak = *hist.iter().max().unwrap();
        if peak > best_peak {
            best_peak = peak;
            best_step = step;
        }
    }
    best_step
}

/// Decode an E-page with an explicit bv (the inverse of the AP stream cipher).
///
/// Returns a 4096-byte buffer where bytes `[0, 0xFF0)` are the decoded body
/// and bytes `[0xFF0, 0x1000)` are the trailer copied verbatim from `raw`.
pub fn deobfuscate_with_bv(raw: &[u8], pn: u64, bv: u8) -> Vec<u8> {
    assert_eq!(raw.len(), PAGE);
    let mut out = vec![0u8; PAGE];
    let p16 = (pn % 16) as u8;
    let bias = p16 / 2 * 4;

    for si in 0..SECTORS {
        let off = si * SECTOR;
        let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
        let sec = &raw[off..end];
        let base = bv
            .wrapping_add(pn as u8)
            .wrapping_add(si as u8)
            .wrapping_sub(bias);
        let step = recover_step(sec, base);
        for (i, &b) in sec.iter().enumerate() {
            out[off + i] = b
                .wrapping_sub(base)
                .wrapping_sub((i as u8).wrapping_mul(step));
        }
    }
    out[TRAILER_START..PAGE].copy_from_slice(&raw[TRAILER_START..PAGE]);
    out
}
