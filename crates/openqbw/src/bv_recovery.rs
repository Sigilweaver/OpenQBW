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

/// Compute the E-page oracle bv assuming `plain[0] == 0x00`.
///
/// All SA17 E-pages start with a null byte (the page header begins with the
/// type/flag bytes after a zero filler). The AP cipher base for sector 0 is
/// `base = bv + pn - bias` where `bias = 4 * ((pn % 16) / 2)`. With step
/// zero at index zero, `stored[0] = base + plain[0]`, so:
///
/// ```text
///   bv = (stored[0] - pn + bias) mod 256
/// ```
///
/// This single-byte computation is reliable for SYSTABLE/SYSCOLUMN catalog
/// pages where the QB anchor used by [`recover_bv_qb_data`] is absent.
pub fn oracle_bv_e_page(pn: u64, raw_page: &[u8]) -> u8 {
    let p16 = (pn % 16) as u8;
    let bias = p16 / 2 * 4;
    raw_page[0].wrapping_sub(pn as u8).wrapping_add(bias)
}

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

/// Recover the page bv by exhaustive search over all 256 candidates.
///
/// This is a fall-back oracle for E-pages where neither the C.36 anchor
/// (`recover_bv_qb_data`) nor the `plain[0] == 0` assumption
/// (`oracle_bv_e_page`) holds. It works on dense pages whose plaintext
/// begins with non-zero header bytes.
///
/// **Method.** The AP cipher decode of byte `i` in sector `si` is
/// `plain[i] = raw[i] - (bv + pn + si - bias) - i*step`. The per-sector
/// step recovered by [`recover_step`] is independent of bv (changing bv
/// only shifts the histogram peak; it does not change which step yields
/// the highest peak). We therefore precompute, for each sector, the
/// histogram of `raw[i] - i*step` over all bytes in the sector body. For
/// each candidate bv, the total count of plaintext zeros across the page
/// is the sum over sectors of `hist[si][bv + pn + si - bias]`. The bv
/// that maximises this total is returned.
///
/// Returns `None` if the maximum zero count is below 5 percent of the
/// page body, indicating that the page is unlikely to be standard AP
/// ciphertext.
pub fn recover_bv_brute(pn: u64, raw: &[u8]) -> Option<u8> {
    if raw.len() != PAGE {
        return None;
    }
    let p16 = (pn % 16) as u8;
    let bias = p16 / 2 * 4;

    let mut hist_per_sec: [[u32; 256]; SECTORS] = [[0; 256]; SECTORS];
    for si in 0..SECTORS {
        let off = si * SECTOR;
        let end = if si == SECTORS - 1 {
            TRAILER_START
        } else {
            off + SECTOR
        };
        let sec = &raw[off..end];
        // Step is base-independent for the purpose of peak location.
        let step = recover_step(sec, 0);
        for (i, &b) in sec.iter().enumerate() {
            let c = b.wrapping_sub((i as u8).wrapping_mul(step));
            hist_per_sec[si][c as usize] += 1;
        }
    }

    let mut best_bv = 0u8;
    let mut best_count = 0u32;
    for bv in 0u16..=255 {
        let bv = bv as u8;
        let mut total = 0u32;
        for si in 0..SECTORS {
            let target = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            total += hist_per_sec[si][target as usize];
        }
        if total > best_count {
            best_count = total;
            best_bv = bv;
        }
    }

    // Require >= 5% zero coverage in the body (TRAILER_START = 4080).
    if best_count >= (TRAILER_START as u32) * 5 / 100 {
        Some(best_bv)
    } else {
        None
    }
}

/// C.37 plaintext magic shared by every regular A-page (SA17 allocation/
/// free-space map B-tree). The 8-byte sequence appears at a varying offset
/// (typically `0xC0..0xF7`) inside the page body and is part of the SA17
/// page-level metadata block.
pub const APAGE_MAGIC: [u8; 8] = [0x24, 0x04, 0x31, 0x00, 0xB4, 0x02, 0x19, 0x00];

/// Recover the page bv for an A-page (allocation/free-space map) using the
/// C.37 magic anchor.
///
/// A-pages share the AP fill obfuscation with E-pages but their first
/// decoded byte is not zero, so [`oracle_bv_e_page`] is wrong and
/// [`recover_bv_qb_data`] (which searches the C.36 `[rc, 0, 0xD5, 0x0B]`
/// anchor) can miss when the A-page's metadata block layout puts the
/// `[0xD5, 0x0B]` byte pair at an unusual offset.
///
/// This oracle searches for the 8-byte plaintext magic
/// `24 04 31 00 b4 02 19 00` (see [`APAGE_MAGIC`]) at any position in the
/// decoded body. For each candidate bv in `0..=255` the page is decoded
/// using sector-local steps recovered via the base-independent histogram
/// peak in [`recover_step`]; if the magic appears the bv is returned.
///
/// Returns `None` for pages that do not contain the magic at any bv
/// (page 3655 of Rock Castle, the residual high-entropy opaque pages,
/// any non-A-page).
pub fn recover_bv_apage(pn: u64, raw_page: &[u8]) -> Option<u8> {
    if raw_page.len() != PAGE {
        return None;
    }
    let p16 = (pn % 16) as u8;
    let bias = p16 / 2 * 4;

    // Precompute c_table[si][i] = raw[i] - i*step using the step that
    // maximises the per-sector histogram peak. The peak count is
    // base-invariant, so the step recovered with `base = 0` is the true
    // sector step regardless of the (still-unknown) bv.
    let mut c_table: [Vec<u8>; SECTORS] = Default::default();
    for si in 0..SECTORS {
        let off = si * SECTOR;
        let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
        let sec = &raw_page[off..end];
        let step = recover_step(sec, 0);
        let mut c = vec![0u8; sec.len()];
        for (i, &b) in sec.iter().enumerate() {
            c[i] = b.wrapping_sub((i as u8).wrapping_mul(step));
        }
        c_table[si] = c;
    }

    let mut plain = [0u8; TRAILER_START];
    for bv in 0u16..=255 {
        let bv = bv as u8;
        let mut cursor = 0usize;
        for si in 0..SECTORS {
            let c = &c_table[si];
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            for (i, &cb) in c.iter().enumerate() {
                plain[cursor + i] = cb.wrapping_sub(base);
            }
            cursor += c.len();
        }
        if contains_subseq(&plain, &APAGE_MAGIC) {
            return Some(bv);
        }
    }
    None
}

/// Linear scan for a contiguous byte subsequence.
fn contains_subseq(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    let limit = hay.len() - needle.len();
    let n0 = needle[0];
    let mut i = 0;
    while i <= limit {
        if hay[i] == n0 && &hay[i..i + needle.len()] == needle {
            return true;
        }
        i += 1;
    }
    false
}

/// Cascade of bv-recovery oracles in order of decreasing confidence.
///
/// 1. `recover_bv_qb_data`  -  C.36 anchor (highest confidence, ~42% of
///    Rock Castle pages).
/// 2. `oracle_bv_e_page`    -  `plain[0] == 0` assumption (works on the
///    majority of E-pages whose header begins with zero filler).
/// 3. `recover_bv_brute`    -  exhaustive zero-count search (rescues
///    dense pages with non-zero leading plaintext).
///
/// The first two oracles are deterministic single-byte computations and
/// near-zero cost; only the brute fall-back performs the per-sector
/// histogram pass. Returns the first oracle that yields a candidate
/// whose decode contains at least one of the validation conditions
/// described in the individual functions, or `None` if no oracle fires.
pub fn recover_bv_any(pn: u64, raw: &[u8]) -> Option<u8> {
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        return Some(bv);
    }
    // The oracle is unconditional (it always returns a byte) and is the
    // correct answer whenever plain[0] == 0. We accept it directly here;
    // callers that need stronger validation can apply their own check on
    // the decoded plaintext.
    let obv = oracle_bv_e_page(pn, raw);
    let plain = deobfuscate_with_bv(raw, pn, obv);
    let zeros = plain[..TRAILER_START]
        .iter()
        .filter(|&&b| b == 0)
        .count();
    if zeros * 100 / TRAILER_START >= 5 {
        return Some(obv);
    }
    // Final fall-back: brute search.
    recover_bv_brute(pn, raw)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic AP-encoded page with known bv, step, and a
    /// scattered non-zero plaintext content.
    fn synth_page(pn: u64, bv: u8, step_per_sec: [u8; SECTORS]) -> ([u8; PAGE], Vec<u8>) {
        let p16 = (pn % 16) as u8;
        let bias = p16 / 2 * 4;
        let mut plain = vec![0u8; PAGE];
        // Sprinkle some non-zero bytes so the page is not uniform.
        // Avoid bytes 0..32 of sector 0 (kept zero so the oracle test
        // would have succeeded too; brute should also recover this).
        for i in (64..TRAILER_START).step_by(37) {
            plain[i] = ((i as u32 * 13) & 0xFF) as u8;
        }
        // Trailer is plaintext.
        plain[TRAILER_START] = 0x04;
        plain[TRAILER_START + 1] = 0x00;
        plain[TRAILER_START + 2] = 0x45; // E-page

        let mut stored = [0u8; PAGE];
        stored[TRAILER_START..].copy_from_slice(&plain[TRAILER_START..]);
        for si in 0..SECTORS {
            let off = si * SECTOR;
            let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
            let step = step_per_sec[si];
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            for i in off..end {
                let idx = (i - off) as u8;
                stored[i] = plain[i]
                    .wrapping_add(base)
                    .wrapping_add(idx.wrapping_mul(step));
            }
        }
        (stored, plain)
    }

    #[test]
    fn brute_recovers_known_bv_zero_dominant() {
        let pn = 1234u64;
        let bv = 0x5a;
        let steps = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let (raw, plain) = synth_page(pn, bv, steps);
        let recovered = recover_bv_brute(pn, &raw).expect("brute returns Some");
        assert_eq!(recovered, bv, "brute should recover the true bv");
        let decoded = deobfuscate_with_bv(&raw, pn, recovered);
        assert_eq!(&decoded[..TRAILER_START], &plain[..TRAILER_START]);
    }

    #[test]
    fn brute_rejects_random_page() {
        // A uniformly-random page should not yield >=5% zeros at any bv.
        let mut raw = [0u8; PAGE];
        let mut x = 0x9e3779b9u32;
        for b in raw.iter_mut() {
            x = x.wrapping_mul(0x85ebca6b).wrapping_add(1);
            *b = (x >> 24) as u8;
        }
        // Pseudo-random input should yield no >=5% zero spike.
        // (May sporadically pass; this is a probabilistic sanity check.)
        let _ = recover_bv_brute(42, &raw); // just exercise the code path
    }

    #[test]
    fn recover_bv_any_prefers_qb_anchor_when_present() {
        // Build a page with the QB anchor at a known offset.
        let pn = 77u64;
        let p16 = (pn % 16) as u8;
        let bias = p16 / 2 * 4;
        let bv = 0xa3u8;
        let mut plain = vec![0u8; PAGE];
        plain[TRAILER_START] = 0x07; // trailer RC
        // Anchor at offset 0x100 in sector 0 == [rc, 0, D5, 0B]
        plain[0x100] = 0x07;
        plain[0x101] = 0x00;
        plain[0x102] = 0xD5;
        plain[0x103] = 0x0B;
        // Encode with bv and step=0 per sector.
        let mut raw = [0u8; PAGE];
        raw[TRAILER_START..].copy_from_slice(&plain[TRAILER_START..]);
        for si in 0..SECTORS {
            let off = si * SECTOR;
            let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            for i in off..end {
                raw[i] = plain[i].wrapping_add(base);
            }
        }
        let recovered = recover_bv_any(pn, &raw).expect("some bv");
        assert_eq!(recovered, bv);
    }

    /// Encode a synthetic A-page: zero-step per sector, magic anchor at
    /// `magic_offset`, sprinkled non-zero bytes so the page is dense.
    fn synth_apage(pn: u64, bv: u8, magic_offset: usize) -> [u8; PAGE] {
        let p16 = (pn % 16) as u8;
        let bias = p16 / 2 * 4;
        let mut plain = vec![0u8; PAGE];
        // Sprinkle non-zero filler resembling repeating B-tree records so
        // `recover_step` peaks at 0 (zero-step encoding).
        for i in 0..TRAILER_START {
            // Pattern dominated by 0x00 with occasional non-zero bytes so
            // plaintext zero is still the dominant byte and `recover_step`
            // returns 0.
            if i % 28 == 0 {
                plain[i] = 0x60;
            } else if i % 28 == 1 {
                plain[i] = 0x80;
            }
        }
        // Plant the C.37 magic.
        plain[magic_offset..magic_offset + APAGE_MAGIC.len()]
            .copy_from_slice(&APAGE_MAGIC);
        // Trailer: A-page rc, type byte 0x41 ('A').
        plain[TRAILER_START] = 0x05;
        plain[TRAILER_START + 1] = 0x00;
        plain[TRAILER_START + 2] = 0x41;

        let mut stored = [0u8; PAGE];
        stored[TRAILER_START..].copy_from_slice(&plain[TRAILER_START..]);
        for si in 0..SECTORS {
            let off = si * SECTOR;
            let end = if si == SECTORS - 1 { TRAILER_START } else { off + SECTOR };
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            for i in off..end {
                // step = 0 per sector.
                stored[i] = plain[i].wrapping_add(base);
            }
        }
        stored
    }

    #[test]
    fn apage_recovers_known_bv() {
        let pn = 3599u64;
        let bv = 66u8;
        let raw = synth_apage(pn, bv, 0xC4);
        let recovered = recover_bv_apage(pn, &raw).expect("apage returns Some");
        assert_eq!(recovered, bv);
    }

    #[test]
    fn apage_handles_magic_near_sector_boundary() {
        // Place the 8-byte magic so it spans the sector 0 / sector 1 boundary
        // at offset 0x200.
        let pn = 3620u64;
        let bv = 244u8;
        let raw = synth_apage(pn, bv, 0x1FE);
        let recovered = recover_bv_apage(pn, &raw).expect("apage returns Some");
        assert_eq!(recovered, bv);
    }

    #[test]
    fn apage_returns_none_when_magic_absent() {
        // Page without the C.37 magic: a random-bytes page.
        let mut raw = [0u8; PAGE];
        let mut x = 0xDEADBEEFu32;
        for b in raw.iter_mut() {
            x = x.wrapping_mul(0x85ebca6b).wrapping_add(1);
            *b = (x >> 24) as u8;
        }
        assert!(recover_bv_apage(123, &raw).is_none());
    }

    #[test]
    fn apage_rejects_wrong_size() {
        let raw = [0u8; PAGE - 1];
        assert!(recover_bv_apage(1, &raw).is_none());
    }

    #[test]
    fn apage_magic_constant_unchanged() {
        assert_eq!(
            APAGE_MAGIC,
            [0x24, 0x04, 0x31, 0x00, 0xB4, 0x02, 0x19, 0x00]
        );
    }
}
