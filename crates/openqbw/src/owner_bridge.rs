//! Bridge [`SysColumn::owner_object_id`] to [`SysTableEntry::name`] using
//! the table-id back-reference carried by the `SYSTABLE` row itself
//! (Phase 6, WP-6Z.4).
//!
//! [`crate::sysobject`] bridges owners to table names by looking for a
//! `SYSOBJECT`-style `<object_id><20 bytes><name_len><name>` row whose
//! `name` is a known table. That scan only resolves the owners of tables
//! that are *named twice* in the file, so on the TLR 2021/2022 practice
//! files it reaches roughly a fifth of the catalog: on `B22_Sample.qbw`
//! it bridges 144 of 659 table names, and `abmc_invoice_header` - whose
//! name appears exactly once in the whole file, in its own `SYSTABLE`
//! row - is not one of them (openqbw#19).
//!
//! The `SYSTABLE` row stores that id directly. Thirty bytes *before* the
//! 16-byte row tag (i.e. 30 bytes before `SysTableEntry::row_offset`,
//! inside the row prefix that section 5 of `SPECIFICATION.md` notes the
//! tag-anchored scan deliberately skips) sits a `u32_LE` that is drawn
//! from the same integer space as [`SysColumn::owner_object_id`]:
//!
//! ```text
//! ... <table_id u32_LE> ..26 bytes.. 05 00 00 00 <tid u32_LE> 00 00 00 00
//!     ^ row_offset - 30                          ^ row_offset
//!     <magic 4B> 00 00 00 00 <name_len u8> <name>
//! ```
//!
//! Note that this `table_id` is *not* `SysTableEntry::table_id`: the two
//! are different namespaces (see the module docs of [`crate::sysobject`]).
//! The back-reference is the compact per-table counter that `SYSCOLUMN`
//! rows point at; `SysTableEntry::table_id` grows by roughly one per
//! *column* in the file and lines up with the `object_id` space.
//!
//! The distance is **calibrated per file** rather than hard-coded, by
//! trying every distance in [`BACKREF_MIN_DISTANCE`]`..=`[`BACKREF_MAX_DISTANCE`]
//! and keeping the one that yields the most distinct `(table name, owner)`
//! pairs whose owner is a non-zero id that `SYSCOLUMN` actually uses.
//! On all six TLR files checked so far (2021 and 2022, sample and
//! chapter files) the winner is [`DEFAULT_BACKREF_DISTANCE`] by a wide
//! margin - it resolves 400+ tables per file against a runner-up of
//! under 45 - so the calibration is there to survive a layout change in
//! another QuickBooks version, not to paper over an ambiguous signal.
//!
//! The resulting map is checked three ways on the corpus: it is
//! near-injective (on `B22_Sample.qbw`, 421 distinct owners for 422
//! names), it is monotone against `SysTableEntry::table_id` (11
//! inversions in 422 pairs), and where it overlaps the independent
//! `SYSOBJECT` scan the two agree (120 of 125 shared names).

use std::collections::{BTreeMap, HashMap, HashSet};

use opensqlany::{ApModel, PageStore};

use crate::bv_recovery::{deobfuscate_with_bv, recover_bv_any};
use crate::syscolumn::SysColumn;
use crate::systable::SysTableEntry;

/// Distance in bytes before the `SYSTABLE` row tag at which the row
/// stores the id that `SYSCOLUMN` rows carry as
/// [`SysColumn::owner_object_id`]. Observed on every TLR 2021/2022 file
/// tested; used as the default when calibration has nothing to work
/// with.
pub const DEFAULT_BACKREF_DISTANCE: usize = 30;

/// Smallest distance considered when calibrating the back-reference.
/// Anything closer overlaps the row tag itself.
pub const BACKREF_MIN_DISTANCE: usize = 4;

/// Largest distance considered when calibrating the back-reference.
pub const BACKREF_MAX_DISTANCE: usize = 96;

/// Minimum number of distinct `(name, owner)` pairs a candidate distance
/// must produce before it is trusted as this file's back-reference.
const MIN_CALIBRATION_PAIRS: usize = 4;

/// Where a given owner -> table-name mapping came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeSource {
    /// The `SYSTABLE` row's own back-reference (this module).
    SysTableBackref,
    /// The `SYSOBJECT` name scan ([`crate::sysobject`]).
    SysObjectScan,
}

impl BridgeSource {
    /// Short human-readable label, for CLI diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            BridgeSource::SysTableBackref => "SYSTABLE back-reference",
            BridgeSource::SysObjectScan => "SYSOBJECT scan",
        }
    }
}

/// Owner -> table-name map, plus how it was obtained.
#[derive(Debug, Clone, Default)]
pub struct OwnerBridge {
    entries: BTreeMap<u32, (String, BridgeSource)>,
    /// Back-reference distance calibrated for this file, if one was
    /// found.
    pub backref_distance: Option<usize>,
    /// Owners mapped from the `SYSTABLE` back-reference.
    pub from_backref: usize,
    /// Owners mapped from the `SYSOBJECT` scan.
    pub from_sysobject: usize,
    /// Owners for which the back-reference saw more than one candidate
    /// table name and had to take a majority vote.
    pub ambiguous_owners: usize,
}

impl OwnerBridge {
    /// Table name bridged to `owner`, if any.
    pub fn table_for(&self, owner: u32) -> Option<&str> {
        self.entries.get(&owner).map(|(n, _)| n.as_str())
    }

    /// Where the mapping for `owner` came from.
    pub fn source_for(&self, owner: u32) -> Option<BridgeSource> {
        self.entries.get(&owner).map(|(_, s)| *s)
    }

    /// Every owner bridged to `table`, ascending. Normally one, but the
    /// scan is heuristic and a name can attract more than one owner.
    pub fn owners_for(&self, table: &str) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|(_, (n, _))| n == table)
            .map(|(o, _)| *o)
            .collect()
    }

    /// Number of bridged owners.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when nothing could be bridged.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of distinct table names covered.
    pub fn tables(&self) -> usize {
        self.entries
            .values()
            .map(|(n, _)| n.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    /// Insert `owner -> name` unless `owner` is already mapped. Returns
    /// `true` when the entry was new.
    fn insert(&mut self, owner: u32, name: String, source: BridgeSource) -> bool {
        if self.entries.contains_key(&owner) {
            return false;
        }
        self.entries.insert(owner, (name, source));
        match source {
            BridgeSource::SysTableBackref => self.from_backref += 1,
            BridgeSource::SysObjectScan => self.from_sysobject += 1,
        }
        true
    }
}

/// Read the `u32_LE` stored `distance` bytes before `row_offset`.
fn backref_at(plain: &[u8], row_offset: usize, distance: usize) -> Option<u32> {
    let start = row_offset.checked_sub(distance)?;
    let bytes = plain.get(start..start + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Per-distance tally of `(table name, owner)` sightings, used to pick
/// this file's back-reference distance and then to build the map.
#[derive(Debug, Clone, Default)]
pub struct BackrefVotes {
    /// distance -> owner -> name -> sightings
    votes: BTreeMap<usize, HashMap<u32, HashMap<String, u32>>>,
    rows_seen: usize,
}

impl BackrefVotes {
    /// Empty tally.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record every candidate back-reference for the `SYSTABLE` rows of
    /// one decoded page.
    ///
    /// `rows` gives `(row_offset, table_name)` for each row found on the
    /// page; `owners` is the set of ids `SYSCOLUMN` rows actually use. A
    /// candidate only counts when it names a non-zero owner from that
    /// set, which is what keeps the zero padding inside the row from
    /// winning the calibration.
    pub fn observe_page(&mut self, plain: &[u8], rows: &[(usize, &str)], owners: &HashSet<u32>) {
        for (row_offset, name) in rows {
            self.rows_seen += 1;
            for distance in BACKREF_MIN_DISTANCE..=BACKREF_MAX_DISTANCE {
                let Some(owner) = backref_at(plain, *row_offset, distance) else {
                    continue;
                };
                if owner == 0 || !owners.contains(&owner) {
                    continue;
                }
                *self
                    .votes
                    .entry(distance)
                    .or_default()
                    .entry(owner)
                    .or_default()
                    .entry((*name).to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    /// `SYSTABLE` rows observed so far.
    pub fn rows_seen(&self) -> usize {
        self.rows_seen
    }

    /// Number of distinct `(name, owner)` pairs a distance yields.
    fn score(&self, distance: usize) -> usize {
        self.votes
            .get(&distance)
            .map(|by_owner| by_owner.values().map(HashMap::len).sum())
            .unwrap_or(0)
    }

    /// Distance that uniquely explains the most `(name, owner)` pairs.
    /// Equally supported layouts are unresolved, allowing the caller to
    /// fall back to the independent `SYSOBJECT` scan.
    pub fn best_distance(&self) -> Option<usize> {
        let best = self.votes.keys().copied().max_by_key(|d| self.score(*d))?;
        let best_score = self.score(best);
        let tied = self
            .votes
            .keys()
            .any(|&distance| distance != best && self.score(distance) == best_score);
        (best_score >= MIN_CALIBRATION_PAIRS && !tied).then_some(best)
    }

    /// Build the owner -> name map for `distance`, resolving an owner
    /// that saw several names by majority vote (ties by name, for
    /// determinism).
    pub fn bridge_at(&self, distance: usize) -> OwnerBridge {
        let mut bridge = OwnerBridge {
            backref_distance: Some(distance),
            ..OwnerBridge::default()
        };
        let Some(by_owner) = self.votes.get(&distance) else {
            return bridge;
        };
        for (owner, names) in by_owner {
            if names.len() > 1 {
                bridge.ambiguous_owners += 1;
            }
            let Some((name, _)) = names
                .iter()
                .max_by_key(|(n, c)| (**c, std::cmp::Reverse(*n)))
            else {
                continue;
            };
            bridge.insert(*owner, name.clone(), BridgeSource::SysTableBackref);
        }
        bridge
    }
}

/// Bridge `SYSCOLUMN` owners to table names using only the `SYSTABLE`
/// back-reference.
///
/// Decodes just the pages that carry `SYSTABLE` rows - a few dozen on a
/// typical file - so this is far cheaper than the whole-file
/// [`crate::sysobject::bridge_owners_to_tables`] scan.
pub fn bridge_owners_via_backref(
    store: &PageStore,
    model: &ApModel,
    columns: &[SysColumn],
    tables: &[SysTableEntry],
) -> OwnerBridge {
    let owners: HashSet<u32> = columns
        .iter()
        .map(|c| c.owner_object_id)
        .filter(|o| *o != 0)
        .collect();
    if owners.is_empty() || tables.is_empty() {
        return OwnerBridge::default();
    }

    let mut by_page: BTreeMap<u64, Vec<(usize, &str)>> = BTreeMap::new();
    for t in tables {
        by_page
            .entry(t.page_number)
            .or_default()
            .push((t.row_offset, t.name.as_str()));
    }

    let mut votes = BackrefVotes::new();
    for (pn, rows) in &by_page {
        let Ok(page) = store.page(*pn) else { continue };
        let raw = page.bytes();
        let plain = if let Some(bv) = recover_bv_any(*pn, raw) {
            deobfuscate_with_bv(raw, *pn, bv)
        } else {
            model.deobfuscate_with_store(raw, *pn, store)
        };
        votes.observe_page(&plain, rows, &owners);
    }

    match votes.best_distance() {
        Some(distance) => votes.bridge_at(distance),
        None => OwnerBridge::default(),
    }
}

/// Merge the `SYSOBJECT` name scan into `bridge`, filling in owners the
/// back-reference did not resolve. Existing entries win: the
/// back-reference is calibrated against this file, the name scan is not.
pub fn extend_with_sysobject(
    bridge: &mut OwnerBridge,
    store: &PageStore,
    model: &ApModel,
    columns: &[SysColumn],
    tables: &[SysTableEntry],
) {
    let scanned = crate::sysobject::bridge_owners_to_tables(store, model, columns, tables);
    for (owner, name) in scanned {
        bridge.insert(owner, name, BridgeSource::SysObjectScan);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay a `SYSTABLE`-shaped row into `page` at `tag`, with `owner`
    /// planted `DEFAULT_BACKREF_DISTANCE` bytes before the tag.
    fn plant_row(page: &mut [u8], tag: usize, owner: u32, tid: u32, name: &str) {
        page[tag - DEFAULT_BACKREF_DISTANCE..tag - DEFAULT_BACKREF_DISTANCE + 4]
            .copy_from_slice(&owner.to_le_bytes());
        page[tag..tag + 4].copy_from_slice(&5u32.to_le_bytes());
        page[tag + 4..tag + 8].copy_from_slice(&tid.to_le_bytes());
        page[tag + 20] = name.len() as u8;
        page[tag + 21..tag + 21 + name.len()].copy_from_slice(name.as_bytes());
    }

    fn owner_set(owners: &[u32]) -> HashSet<u32> {
        owners.iter().copied().collect()
    }

    #[test]
    fn backref_reads_the_u32_before_the_row() {
        let mut page = vec![0u8; 256];
        page[100..104].copy_from_slice(&3073u32.to_le_bytes());
        assert_eq!(backref_at(&page, 130, 30), Some(3073));
        assert_eq!(backref_at(&page, 130, 29), Some(3073 >> 8));
        // A distance that would run off the front of the page is not a
        // candidate rather than a panic.
        assert_eq!(backref_at(&page, 8, 30), None);
    }

    #[test]
    fn calibration_finds_the_planted_distance() {
        let mut page = vec![0u8; 0x1000];
        plant_row(&mut page, 200, 3073, 5884, "abmc_invoice_header");
        plant_row(&mut page, 400, 3074, 5948, "abmc_invoice_lineitem");
        plant_row(&mut page, 600, 3075, 6080, "abmc_item_assembly_header");
        plant_row(&mut page, 800, 3076, 6113, "abmc_item_history");

        let rows = [
            (200usize, "abmc_invoice_header"),
            (400, "abmc_invoice_lineitem"),
            (600, "abmc_item_assembly_header"),
            (800, "abmc_item_history"),
        ];
        let mut votes = BackrefVotes::new();
        votes.observe_page(&page, &rows, &owner_set(&[3073, 3074, 3075, 3076]));

        assert_eq!(votes.rows_seen(), 4);
        assert_eq!(votes.best_distance(), Some(DEFAULT_BACKREF_DISTANCE));

        let bridge = votes.bridge_at(DEFAULT_BACKREF_DISTANCE);
        assert_eq!(bridge.table_for(3073), Some("abmc_invoice_header"));
        assert_eq!(bridge.table_for(3076), Some("abmc_item_history"));
        assert_eq!(bridge.owners_for("abmc_invoice_header"), vec![3073]);
        assert_eq!(bridge.from_backref, 4);
        assert_eq!(bridge.ambiguous_owners, 0);
        assert_eq!(bridge.source_for(3073), Some(BridgeSource::SysTableBackref));
    }

    #[test]
    fn calibration_follows_a_shifted_layout() {
        // Same rows, but this file keeps the id 44 bytes before the tag.
        let shift = 44usize;
        let mut page = vec![0u8; 0x1000];
        for (i, (tag, owner)) in [(200usize, 900u32), (400, 901), (600, 902), (800, 903)]
            .into_iter()
            .enumerate()
        {
            page[tag - shift..tag - shift + 4].copy_from_slice(&owner.to_le_bytes());
            let _ = i;
        }
        let rows = [(200usize, "t_a"), (400, "t_b"), (600, "t_c"), (800, "t_d")];
        let mut votes = BackrefVotes::new();
        votes.observe_page(&page, &rows, &owner_set(&[900, 901, 902, 903]));

        assert_eq!(votes.best_distance(), Some(shift));
        assert_eq!(votes.bridge_at(shift).table_for(902), Some("t_c"));
    }

    #[test]
    fn equally_supported_offsets_leave_sysobject_fallback_available() {
        let mut plain = vec![0u8; 0x1000];
        let rows = [
            (200usize, "table_a"),
            (400, "table_b"),
            (600, "table_c"),
            (800, "table_d"),
        ];
        let mut columns = Vec::new();
        let mut tables = Vec::new();
        for (i, (tag, name)) in rows.iter().enumerate() {
            let owner = 900 + i as u32;
            plant_row(
                &mut plain,
                *tag,
                900 + ((i + 1) % 4) as u32,
                100 + i as u32,
                name,
            );
            // One plausible field at -30 conflicts with the intended -44
            // layout. Both explain four distinct known-owner pairs.
            plain[tag - 44..tag - 40].copy_from_slice(&owner.to_le_bytes());
            columns.push(SysColumn {
                name: format!("column_{i}"),
                owner_object_id: owner,
                column_id: 1,
                nulls_flag: 0,
                domain_char: b'N',
                width: 4,
                page_number: 0,
                tag_offset: 0,
            });
            tables.push(SysTableEntry {
                table_id: 100 + i as u32,
                name: (*name).into(),
                magic: [0; 4],
                col_count: None,
                data_root_page: None,
                last_page: None,
                page_number: 0,
                row_offset: *tag,
            });
            // Independent SYSOBJECT-shaped records establish the correct
            // mapping, without depending on either prefix layout.
            let start = 1000 + i * 200;
            plain[start..start + 4].copy_from_slice(&owner.to_le_bytes());
            plain[start + 24] = name.len() as u8;
            plain[start + 25..start + 25 + name.len()].copy_from_slice(name.as_bytes());
        }
        let owners = owner_set(&[900, 901, 902, 903]);
        let mut votes = BackrefVotes::new();
        votes.observe_page(&plain, &rows, &owners);
        assert_eq!(votes.score(30), 4);
        assert_eq!(votes.score(44), 4);
        assert_eq!(votes.best_distance(), None);

        // Encode a zero-step, bv=0 synthetic AP page, using page zero so
        // each sector's base is its sector index. The trailer is plaintext.
        plain[0xFF2] = b'E';
        let mut raw = plain.clone();
        for (offset, byte) in raw[..0xFF0].iter_mut().enumerate() {
            *byte = byte.wrapping_add((offset / 512) as u8);
        }
        let store = PageStore::from_bytes(raw).unwrap();
        let model = ApModel::default();
        let mut bridge = bridge_owners_via_backref(&store, &model, &columns, &tables);
        assert!(bridge.is_empty());
        assert_eq!(bridge.backref_distance, None);
        extend_with_sysobject(&mut bridge, &store, &model, &columns, &tables);
        assert_eq!(bridge.from_backref, 0);
        assert_eq!(bridge.from_sysobject, 4);
        for (i, (_, name)) in rows.iter().enumerate() {
            assert_eq!(bridge.table_for(900 + i as u32), Some(*name));
            assert_eq!(
                bridge.source_for(900 + i as u32),
                Some(BridgeSource::SysObjectScan)
            );
        }
    }

    #[test]
    fn zero_padding_never_wins_calibration() {
        // A file whose SYSCOLUMN owners include ids that happen to sit in
        // the row's zero padding must not calibrate onto that padding.
        let mut page = vec![0u8; 0x1000];
        plant_row(&mut page, 200, 3073, 5884, "abmc_invoice_header");
        plant_row(&mut page, 400, 3074, 5948, "abmc_invoice_lineitem");
        plant_row(&mut page, 600, 3075, 6080, "abmc_item_assembly_header");
        plant_row(&mut page, 800, 3076, 6113, "abmc_item_history");

        let rows = [
            (200usize, "abmc_invoice_header"),
            (400, "abmc_invoice_lineitem"),
            (600, "abmc_item_assembly_header"),
            (800, "abmc_item_history"),
        ];
        let mut votes = BackrefVotes::new();
        // 0 is in the owner set here, as it is on several TLR files.
        votes.observe_page(&page, &rows, &owner_set(&[0, 3073, 3074, 3075, 3076]));
        assert_eq!(votes.best_distance(), Some(DEFAULT_BACKREF_DISTANCE));
    }

    #[test]
    fn too_little_evidence_is_no_bridge() {
        let mut page = vec![0u8; 0x1000];
        plant_row(&mut page, 200, 3073, 5884, "abmc_invoice_header");
        let rows = [(200usize, "abmc_invoice_header")];
        let mut votes = BackrefVotes::new();
        votes.observe_page(&page, &rows, &owner_set(&[3073]));
        assert_eq!(votes.best_distance(), None);
    }

    #[test]
    fn duplicate_rows_vote_rather_than_collide() {
        let mut page = vec![0u8; 0x1000];
        plant_row(&mut page, 200, 3073, 5884, "abmc_invoice_header");
        plant_row(&mut page, 400, 3073, 5884, "abmc_invoice_header");
        plant_row(&mut page, 600, 3074, 5948, "abmc_invoice_lineitem");
        plant_row(&mut page, 800, 3075, 6080, "abmc_item_history");
        // A stray row that claims an already-claimed owner once.
        plant_row(&mut page, 1000, 3073, 4242, "zz_decoy");

        let rows = [
            (200usize, "abmc_invoice_header"),
            (400, "abmc_invoice_header"),
            (600, "abmc_invoice_lineitem"),
            (800, "abmc_item_history"),
            (1000, "zz_decoy"),
        ];
        let mut votes = BackrefVotes::new();
        votes.observe_page(&page, &rows, &owner_set(&[3073, 3074, 3075]));

        let bridge = votes.bridge_at(DEFAULT_BACKREF_DISTANCE);
        assert_eq!(bridge.table_for(3073), Some("abmc_invoice_header"));
        assert_eq!(bridge.ambiguous_owners, 1);
        assert_eq!(bridge.tables(), 3);
    }

    #[test]
    fn sysobject_entries_do_not_displace_backref_entries() {
        let mut bridge = OwnerBridge {
            backref_distance: Some(DEFAULT_BACKREF_DISTANCE),
            ..OwnerBridge::default()
        };
        assert!(bridge.insert(7, "from_backref".into(), BridgeSource::SysTableBackref));
        assert!(!bridge.insert(7, "from_scan".into(), BridgeSource::SysObjectScan));
        assert!(bridge.insert(8, "from_scan".into(), BridgeSource::SysObjectScan));

        assert_eq!(bridge.table_for(7), Some("from_backref"));
        assert_eq!(bridge.from_backref, 1);
        assert_eq!(bridge.from_sysobject, 1);
        assert_eq!(bridge.len(), 2);
    }
}
