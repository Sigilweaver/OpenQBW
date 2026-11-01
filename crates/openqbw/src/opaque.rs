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

use crate::decode::decode_page;
use opensqlany::{ApModel, PageStore, PageType};

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

/// Returns `true` if the decoded page body is indistinguishable from random
/// data, meaning no decoder produced readable content for it.
///
/// `plain_page` is the page after the AP cipher has been removed (the stored
/// bytes are ciphertext and always measure near 8 bits/byte, whatever they
/// hold). Tests only the body region `0..0xFF0`; the trailer is plaintext and
/// would skew the entropy slightly.
pub fn is_opaque_high_entropy(plain_page: &[u8]) -> bool {
    if plain_page.len() < TRAILER_START {
        return false;
    }
    shannon_entropy(&plain_page[..TRAILER_START]) >= OPAQUE_ENTROPY_THRESHOLD
}

/// Share of `E` pages above which a file is treated as unreadable content
/// (for example a password-protected file) rather than a readable file with
/// a few opaque blobs. Not measured on a corpus.
pub const OPAQUE_FILE_FRACTION: f64 = 0.5;

/// Counts of `E` pages in a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct OpaqueCount {
    /// `E` pages whose body is indistinguishable from random bytes.
    pub(crate) opaque: u64,
    /// All `E` pages.
    pub(crate) extent: u64,
}

impl OpaqueCount {
    /// `true` when more than [`OPAQUE_FILE_FRACTION`] of the `E` pages are
    /// opaque.
    pub(crate) fn mostly_opaque(&self) -> bool {
        self.extent > 0 && self.opaque as f64 / self.extent as f64 > OPAQUE_FILE_FRACTION
    }
}

/// Count opaque and total `E` pages in `store`.
///
/// Each page is decoded the way the line-item scan decodes it (`decode_page`: confirmed
/// keystream, QB anchor bv, then the lenient `model` fallback) before its entropy is measured.
pub(crate) fn count_opaque_pages(store: &PageStore, model: &ApModel) -> crate::Result<OpaqueCount> {
    let mut c = OpaqueCount {
        opaque: 0,
        extent: 0,
    };
    for pn in 1..store.page_count() {
        let page = store
            .page(pn)
            .map_err(|e| crate::Error::from_store(e, Some(pn)))?;
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        c.extent += 1;
        // A page no decoder can read is opaque. The line-item scan decodes
        // the same page and reports the decode error itself.
        let readable = match decode_page(store, model, pn) {
            Ok(d) => !is_opaque_high_entropy(d.bytes()),
            Err(_) => false,
        };
        if !readable {
            c.opaque += 1;
        }
    }
    Ok(c)
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
    fn mostly_opaque_needs_more_than_half() {
        let c = |opaque, extent| OpaqueCount { opaque, extent };
        assert!(!c(0, 0).mostly_opaque());
        assert!(!c(5, 10).mostly_opaque());
        assert!(c(6, 10).mostly_opaque());
    }

    #[test]
    fn short_slice_does_not_panic() {
        let raw = [0u8; 100];
        assert!(!is_opaque_high_entropy(&raw));
    }

    /// Encode `plain[..0xFF0]` as an AP-ciphered `E` page: per sector,
    /// `stored = plain + base + i * step` with `base = bv + pn + si - bias`.
    fn encode_e_page(pn: u64, bv: u8, step: u8, plain: &[u8]) -> Vec<u8> {
        let bias = (pn % 16) as u8 / 2 * 4;
        let mut out = vec![0u8; 4096];
        for si in 0..8usize {
            let off = si * 512;
            let end = if si == 7 { TRAILER_START } else { off + 512 };
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add(si as u8)
                .wrapping_sub(bias);
            for i in off..end {
                out[i] = plain[i]
                    .wrapping_add(base)
                    .wrapping_add(((i - off) as u8).wrapping_mul(step));
            }
        }
        out[0xFF2] = b'E';
        out
    }

    #[test]
    fn encoded_readable_pages_are_not_opaque() {
        // Sparse plaintext, as a real data page is, stored under the AP
        // cipher. The cipher ramp makes the stored bytes look uniform, so
        // entropy must be judged on the decoded body.
        let mut plain = vec![0u8; 4096];
        for i in (0..TRAILER_START).step_by(64) {
            plain[i] = 0x07;
        }
        let mut bytes = vec![0u8; 4096];
        for pn in 1..=4u64 {
            bytes.extend(encode_e_page(pn, 0x5a, 7, &plain));
        }
        let store = crate::testutil::sealed_store(bytes);
        let model = crate::testutil::lenient_model(&store);
        let c = count_opaque_pages(&store, &model).unwrap();
        assert_eq!((c.opaque, c.extent), (0, 4));
    }

    #[test]
    fn threshold_is_seventy_five_tenths() {
        // Sanity check the published constant.
        assert!((OPAQUE_ENTROPY_THRESHOLD - 7.5).abs() < 1e-9);
    }
}
