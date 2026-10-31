//! Page decoding shared by every scanner.
//!
//! `opensqlany` 0.3 decodes a page only when something confirms the AP
//! keystream: a trailer row-count match, agreeing exact sectors, or a
//! corroborated histogram. About 72% of QuickBooks `E` pages have none of
//! those (opensqlany CHANGELOG, Unreleased), so an `E` page is decoded in
//! this order:
//!
//! 1. `ApModel::deobfuscate` in strict mode. The keystream is confirmed.
//! 2. The QuickBooks C.36 anchor (`[trailer_RC] 00 D5 0B`): when exactly one
//!    `bv` of the 256 decodes it, that `bv` is taken as confirmed and handed
//!    to `ApModel::deobfuscate_with_bv`.
//! 3. `ApModel::deobfuscate` under `Leniency::KEYSTREAM`. The page decodes,
//!    and every record read from it carries [`RecordFlag::KeystreamUnverified`].
//!
//! Sectors whose step opensqlany could not tell from its runner-up are
//! recorded too; a record whose bytes touch one carries
//! [`RecordFlag::LowConfidenceSector`].

use std::ops::Range;

use opensqlany::{
    ApModel, ErrorKind as SaErrorKind, Leniency, Page, PageStore, ReadOptions, SECTOR_SIZE,
    SECTORS_PER_PAGE, SlottedPage, WarningKind,
};

use crate::bv_recovery::qb_anchor_bv_unique;
use crate::error::Error;
use crate::scan::RecordFlag;

/// A decoded page and how far its bytes can be trusted.
pub(crate) struct DecodedPage {
    pn: u64,
    bytes: Vec<u8>,
    keystream_unverified: bool,
    /// Bit `i` set: sector `i` has a low-confidence step.
    weak_sectors: u8,
}

impl DecodedPage {
    /// Page number.
    pub(crate) fn pn(&self) -> u64 {
        self.pn
    }

    /// The 4096 decoded bytes (trailer and CRC copied verbatim).
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Flags for a record that occupies `range` of the decoded page.
    pub(crate) fn flags_for(&self, range: Range<usize>) -> Vec<RecordFlag> {
        let mut flags = Vec::new();
        if self.keystream_unverified {
            flags.push(RecordFlag::KeystreamUnverified);
        }
        if range.start < range.end {
            let first = (range.start / SECTOR_SIZE).min(SECTORS_PER_PAGE - 1);
            let last = ((range.end - 1) / SECTOR_SIZE).min(SECTORS_PER_PAGE - 1);
            if (first..=last).any(|s| self.weak_sectors & (1 << s) != 0) {
                flags.push(RecordFlag::LowConfidenceSector);
            }
        }
        flags
    }

    /// Parse the slot directory in strict mode. `Ok(None)` when the page has
    /// no directory, or when opensqlany cannot confirm one (a structure
    /// error); callers that need rows count such pages.
    pub(crate) fn slotted(&self) -> Result<Option<SlottedPage<'_>>, Error> {
        let page = Page::from_bytes(self.pn, &self.bytes)
            .map_err(|e| Error::from_store(e, Some(self.pn)))?;
        match SlottedPage::parse(page, &ReadOptions::strict()) {
            Ok(sp) if sp.directory().is_some() => Ok(Some(sp)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == SaErrorKind::Structure => Ok(None),
            Err(e) => Err(Error::from_store(e, Some(self.pn))),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_plain(pn: u64, bytes: Vec<u8>) -> Self {
        Self {
            pn,
            bytes,
            keystream_unverified: false,
            weak_sectors: 0,
        }
    }
}

/// Decode page `pn` of `store`. See the module documentation for the order
/// in which keystreams are tried.
pub(crate) fn decode_page(
    store: &PageStore,
    model: &ApModel,
    pn: u64,
) -> Result<DecodedPage, Error> {
    let map = |e| Error::from_store(e, Some(pn));
    match model.deobfuscate(store, pn, &ReadOptions::strict()) {
        Ok(d) => return Ok(finish(pn, d, true)),
        Err(e) if is_unconfirmed_keystream(&e) => {}
        Err(e) => return Err(map(e)),
    }
    let lenient = ReadOptions::strict().with_leniency(Leniency::KEYSTREAM);
    let page = store.page(pn).map_err(map)?;
    if let Some(bv) = qb_anchor_bv_unique(pn, page.bytes()) {
        let d = ApModel::deobfuscate_with_bv(page, bv, &lenient).map_err(map)?;
        return Ok(finish(pn, d, true));
    }
    let d = model.deobfuscate(store, pn, &lenient).map_err(map)?;
    Ok(finish(pn, d, false))
}

/// Keystream errors that `Leniency::KEYSTREAM` accepts. `StoreMismatch` is
/// a keystream-kind error too, but a caller mistake.
fn is_unconfirmed_keystream(e: &opensqlany::Error) -> bool {
    matches!(
        e,
        opensqlany::Error::KeystreamUnverified { .. }
            | opensqlany::Error::AmbiguousCalibration { .. }
            | opensqlany::Error::LowConfidenceKeystream { .. }
    )
}

fn finish(pn: u64, d: opensqlany::DeobfuscatedPage, bv_confirmed: bool) -> DecodedPage {
    // The lenient fallback path decoded without a confirmed keystream; that
    // holds whether or not opensqlany attached a warning to the page.
    let mut keystream_unverified = !bv_confirmed;
    let mut weak_sectors = 0u8;
    for w in d.warnings() {
        match &w.kind {
            WarningKind::UnverifiedKeystream { .. } | WarningKind::AmbiguousCalibration { .. } => {}
            WarningKind::LowConfidenceSector { .. } => match w.sector {
                Some(s) if s < SECTORS_PER_PAGE => weak_sectors |= 1 << s,
                _ => weak_sectors = u8::MAX,
            },
            // Any other warning on a decoded page means a check was relaxed
            // that this module did not ask for; do not vouch for the page.
            _ => keystream_unverified = true,
        }
    }
    DecodedPage {
        pn,
        bytes: d.into_bytes(),
        keystream_unverified,
        weak_sectors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{lenient_model, sealed_store};

    /// AP-encode `plain` as page `pn` with `bv` and step 0 in every sector.
    fn encode(plain: &[u8], pn: u64, bv: u8) -> Vec<u8> {
        let bias = ((pn % 16) as u8) / 2 * 4;
        let mut raw = plain.to_vec();
        for (i, b) in raw[..0xFF0].iter_mut().enumerate() {
            let base = bv
                .wrapping_add(pn as u8)
                .wrapping_add((i / SECTOR_SIZE) as u8)
                .wrapping_sub(bias);
            *b = b.wrapping_add(base);
        }
        raw
    }

    /// Plaintext `E` page with a few non-zero bytes in every sector, so no
    /// sector is an exact AP progression.
    fn sparse_plain() -> Vec<u8> {
        let mut plain = vec![0u8; 4096];
        for i in (40..0xFF0).step_by(53) {
            plain[i] = 0x31 + (i % 7) as u8;
        }
        plain[0xFF2] = b'E';
        plain
    }

    fn store_with(page1: Vec<u8>) -> PageStore {
        let mut bytes = vec![0u8; 4096];
        bytes.extend(page1);
        sealed_store(bytes)
    }

    #[test]
    fn page_nothing_confirms_is_decoded_and_flagged() {
        let plain = sparse_plain();
        let store = store_with(encode(&plain, 1, 0x5A));
        let model = lenient_model(&store);
        let d = decode_page(&store, &model, 1).unwrap();
        assert!(d.keystream_unverified);
        assert_eq!(d.flags_for(0..10), vec![RecordFlag::KeystreamUnverified]);
    }

    #[test]
    fn unverified_path_is_flagged_without_an_opensqlany_warning() {
        // All-zero sectors are exact AP progressions, so opensqlany accepts
        // the supplied bv without a warning.
        let mut plain = vec![0u8; 4096];
        plain[0xFF2] = b'E';
        let store = store_with(encode(&plain, 1, 0x5A));
        let lenient = ReadOptions::strict().with_leniency(Leniency::KEYSTREAM);
        let page = store.page(1).unwrap();
        let d = ApModel::deobfuscate_with_bv(page, 0x5A, &lenient).unwrap();
        assert!(d.warnings().is_empty(), "test needs a warning-free decode");
        assert!(finish(1, d, false).keystream_unverified);
        let d = ApModel::deobfuscate_with_bv(page, 0x5A, &lenient).unwrap();
        assert!(!finish(1, d, true).keystream_unverified);
    }

    #[test]
    fn unique_qb_anchor_confirms_the_key() {
        let mut plain = sparse_plain();
        plain[0xFF0] = 0x07;
        plain[0x100..0x104].copy_from_slice(&[0x07, 0x00, 0xD5, 0x0B]);
        let store = store_with(encode(&plain, 1, 0x5A));
        let model = lenient_model(&store);
        let d = decode_page(&store, &model, 1).unwrap();
        assert!(!d.keystream_unverified);
        assert_eq!(&d.bytes()[..0xFF0], &plain[..0xFF0]);
    }

    #[test]
    fn record_touching_weak_sector_is_flagged() {
        let mut p = DecodedPage::from_plain(1, vec![0; 4096]);
        p.weak_sectors = 1 << 2;
        assert!(p.flags_for(0..100).is_empty());
        assert_eq!(
            p.flags_for(1000..1030),
            vec![RecordFlag::LowConfidenceSector]
        );
        assert_eq!(
            p.flags_for(1530..1540),
            vec![RecordFlag::LowConfidenceSector]
        );
    }

    #[test]
    fn unverified_page_flags_every_record() {
        let mut p = DecodedPage::from_plain(1, vec![0; 4096]);
        p.keystream_unverified = true;
        assert_eq!(p.flags_for(0..1), vec![RecordFlag::KeystreamUnverified]);
    }
}
