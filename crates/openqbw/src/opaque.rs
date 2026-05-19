//! Opaque-page classification (WP-5C).
//!
//! Some E-pages in QuickBooks `.qbw` files are not standard AP ciphertext:
//! their plaintext is high-entropy (Shannon entropy >= 7.5 bits/byte) under
//! every candidate `bv`, no QB-anchor decoder accepts them, and sibling
//! pages within the same 8-page block - which decode normally - cannot
//! "lend" their `bv` either. The most likely interpretation is that these
//! pages carry compressed or separately-encrypted blobs managed by the
//! SA17 long-data / overflow subsystem, stored verbatim alongside ordinary
//! AP-encoded pages.
//!
//! WP-5C ships an explicit predicate so the verify pipeline can:
//!
//! 1. Account for them as a known sub-class rather than "decode failed";
//! 2. Skip them in line-item / header scans, preventing spurious anchor
//!    hits from random bytes.
//!
//! See `OpenQBW/re/NOTES.md` (entry C.51, WP-5C) for the empirical
//! characterisation across the four-file corpus.

const TRAILER_START: usize = 0xFF0;

/// Shannon entropy of `bytes` in bits/byte (0.0 .. 8.0).
fn shannon_entropy(bytes: &[u8]) -> f64 {
    let mut hist = [0u32; 256];
    for &b in bytes {
        hist[b as usize] += 1;
    }
    let total = bytes.len() as f64;
    if total == 0.0 {
        return 0.0;
    }
    let mut h = 0.0;
    for &c in &hist {
        if c == 0 {
            continue;
        }
        let p = c as f64 / total;
        h -= p * p.log2();
    }
    h
}

/// Threshold (bits/byte) above which the page body is treated as
/// uniformly-random. Empirically every page in the four-file Phase 5 corpus
/// that survives all four bv-recovery tiers scores in `[7.5, 8.0)`; pages
/// that *do* decode have body entropy well below 7.5 once the AP cipher is
/// removed.
pub const OPAQUE_ENTROPY_THRESHOLD: f64 = 7.5;

/// Returns `true` if the raw page bytes are indistinguishable from random
/// data and therefore unlikely to be standard AP ciphertext.
///
/// Tests only the body region `0..0xFF0`; the trailer is plaintext and
/// would skew the entropy slightly.
pub fn is_opaque_high_entropy(raw_page: &[u8]) -> bool {
    if raw_page.len() < TRAILER_START {
        return false;
    }
    shannon_entropy(&raw_page[..TRAILER_START]) >= OPAQUE_ENTROPY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zeros_is_not_opaque() {
        let raw = [0u8; 4096];
        assert!(!is_opaque_high_entropy(&raw));
    }

    #[test]
    fn pseudo_random_is_opaque() {
        let mut raw = [0u8; 4096];
        let mut x: u32 = 0xdeadbeef;
        for b in raw.iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        assert!(is_opaque_high_entropy(&raw));
    }

    #[test]
    fn sparse_page_with_anchor_is_not_opaque() {
        let mut raw = [0u8; 4096];
        // Mostly zeros, a few scattered structural bytes.
        for i in (0..0xFF0).step_by(64) {
            raw[i] = 0x07;
            raw[i + 1] = 0x00;
            raw[i + 2] = 0xD5;
            raw[i + 3] = 0x0B;
        }
        assert!(!is_opaque_high_entropy(&raw));
    }

    #[test]
    fn short_slice_does_not_panic() {
        let raw = [0u8; 100];
        assert!(!is_opaque_high_entropy(&raw));
    }

    #[test]
    fn threshold_is_seventy_five_tenths() {
        // Sanity check the published constant.
        assert!((OPAQUE_ENTROPY_THRESHOLD - 7.5).abs() < 1e-9);
    }
}
