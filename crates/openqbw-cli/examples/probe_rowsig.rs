//! WP-4A recon: extract row-content signatures from known table roots and
//! check whether they are (a) unique per table and (b) stable across pages
//! known to belong to the same table.
//!
//! A "signature" here is the sequence of 2-byte SA17 column type tags read
//! from row 0 of the page (and optionally from rows 1..N). Per C.38, column
//! tags include `0x000E` (QB_ID), `0x000C` (account name), date columns,
//! etc. The tag at byte i is detected by scanning for low-byte type codes
//! that match known SA17 types.
//!
//! Two-phase output:
//!   Phase 1: per-table root signature (first 16 bytes of row 0).
//!   Phase 2: dedup signatures across all 728 user-table roots.
//!            Stability = how many distinct tables share each signature.
//!            If most signatures are unique, WP-4A is viable.

use openqbw::{SysTableEntry, deobfuscate_with_bv, iter_systable_entries, recover_bv_qb_data};
use opensqlany::{ApModel, Page, PageStore, PageType, SlottedPage};
use std::collections::BTreeMap;

fn decode_page(pn: u64, raw: &[u8], model: &ApModel, store: &PageStore) -> Vec<u8> {
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        deobfuscate_with_bv(raw, pn, bv)
    } else {
        model.deobfuscate_with_store(raw, pn, store)
    }
}

fn row_signature(row: &[u8], len: usize) -> Vec<u8> {
    let take = row.len().min(len);
    row[..take].to_vec()
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);

    let entries: Vec<SysTableEntry> = iter_systable_entries(&store, &model).collect();
    let mut by_root: BTreeMap<u32, String> = BTreeMap::new();
    for e in &entries {
        if let Some(root) = e.data_root_page {
            by_root.entry(root).or_insert_with(|| e.name.clone());
        }
    }
    println!("# tables with data_root: {}", by_root.len());

    // For each E-page root, decode and extract row 0 / row 1 signatures.
    let mut sig_to_tables: BTreeMap<Vec<u8>, Vec<String>> = BTreeMap::new();
    let mut no_slot = 0usize;
    let mut empty_rows = 0usize;
    let mut decoded = 0usize;
    let mut samples = Vec::new();

    const SIG_LEN: usize = 12;

    for (&root, name) in &by_root {
        let page = match store.page(root as u64) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        decoded += 1;
        let raw = page.bytes();
        let plain = decode_page(root as u64, raw, &model, &store);
        let p = Page::from_bytes(root as u64, &plain);
        let sp = SlottedPage::parse(p);
        let rows = sp.row_bytes();
        if sp.directory.is_none() {
            no_slot += 1;
            continue;
        }
        if rows.is_empty() {
            empty_rows += 1;
            continue;
        }
        let sig = row_signature(rows[0].1, SIG_LEN);
        sig_to_tables
            .entry(sig.clone())
            .or_default()
            .push(name.clone());
        if samples.len() < 12 {
            samples.push((name.clone(), root, sig.clone(), rows.len(), rows[0].1.len()));
        }
    }

    println!("# E-page roots decoded: {}", decoded);
    println!("# roots with no slot directory: {}", no_slot);
    println!("# roots with empty rows: {}", empty_rows);
    println!(
        "# distinct row-0 signatures (len {}): {}",
        SIG_LEN,
        sig_to_tables.len()
    );

    let collisions: Vec<(usize, Vec<String>)> = sig_to_tables
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(_, v)| (v.len(), v.clone()))
        .collect();
    println!("# signatures shared by >1 table: {}", collisions.len());

    let unique_signatures = sig_to_tables.values().filter(|v| v.len() == 1).count();
    println!(
        "# tables with UNIQUE row-0 signature: {}",
        unique_signatures
    );

    println!("\n## sample roots (table, root_pn, sig_hex, n_rows, row0_len)");
    for (name, root, sig, nrows, row0_len) in &samples {
        let hex: String = sig
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  {:<42}  pn={:>5}  rows={:>3}  r0_len={:>4}  sig=[{}]",
            name, root, nrows, row0_len, hex
        );
    }

    println!("\n## top signature collisions (count >= 2)");
    let mut sorted: Vec<(Vec<u8>, Vec<String>)> = sig_to_tables
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .collect();
    sorted.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (sig, tabs) in sorted.iter().take(8) {
        let hex: String = sig
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  sig=[{}] -> {} tables: {:?}",
            hex,
            tabs.len(),
            &tabs[..tabs.len().min(6)]
        );
    }

    Ok(())
}
