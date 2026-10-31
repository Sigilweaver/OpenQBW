//! Helpers for unit tests that build synthetic page stores.

use opensqlany::{ApModel, Leniency, PageStore, ReadOptions};

/// Write the CRC-32 footer of a 4096-byte page.
pub(crate) fn seal(page: &mut [u8]) {
    let crc = crc32fast::hash(&page[..0xFFC]);
    page[0xFFC..0x1000].copy_from_slice(&crc.to_le_bytes());
}

/// Seal every page of `bytes` and wrap it without superblock checks.
pub(crate) fn sealed_store(mut bytes: Vec<u8>) -> PageStore {
    for p in bytes.chunks_mut(0x1000) {
        seal(p);
    }
    PageStore::from_bytes_unchecked(bytes).unwrap()
}

/// A model for a synthetic store that has no pure-AP calibration pages.
pub(crate) fn lenient_model(store: &PageStore) -> ApModel {
    ApModel::learn(
        store,
        &ReadOptions::strict().with_leniency(Leniency::KEYSTREAM),
    )
    .unwrap()
}
