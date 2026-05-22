//! WP-4C: Cross-corpus triangulation.
//!
//! For each QBW file given on the command line, enumerate user tables via
//! SYSTABLE, decode each table's data_root page, and dump structural
//! constants. Then cross-correlate to find:
//!   (1) format-universal constants (same across all files)
//!   (2) per-table stable signatures (same across files for the same table)
//!   (3) file-specific noise (varies even for the same table)
//!
//! Output is per-table: file -> (root_pn, root_type, header_bytes_hex_16,
//! v06_07, v12_13, row_count_field).

use openqbw::{SysTableEntry, deobfuscate_with_bv, iter_systable_entries, recover_bv_qb_data};
use opensqlany::{ApModel, PageStore, PageType};
use std::collections::BTreeMap;
use std::path::Path;

const D5_0B: [u8; 2] = [0xD5, 0x0B];

fn find_meta(plain: &[u8], pn: u64) -> Option<usize> {
    let target = (pn as u32).to_le_bytes();
    let scan_end = plain.len().min(0xFF0);
    if scan_end < 26 {
        return None;
    }
    let mut i = 24usize;
    while i + 2 <= scan_end {
        if plain[i..i + 2] == D5_0B {
            let so = i - 24;
            if plain[so..so + 4] == target {
                return Some(so);
            }
        }
        i += 1;
    }
    None
}

#[derive(Default, Debug)]
struct PageProfile {
    found: bool,
    page_type: char,
    v06_07: u16,
    v12_13: u16,
    v14: u8,
    row_count: u16,
    header16: [u8; 16],
}

fn profile_root(store: &PageStore, model: &ApModel, root: u32) -> PageProfile {
    let mut p = PageProfile::default();
    let page = match store.page(root as u64) {
        Ok(p) => p,
        Err(_) => return p,
    };
    p.page_type = page.trailer().page_type().as_byte() as char;
    let raw = page.bytes();
    let plain = if let Some(bv) = recover_bv_qb_data(root as u64, raw) {
        deobfuscate_with_bv(raw, root as u64, bv)
    } else {
        model.deobfuscate_with_store(raw, root as u64, store)
    };
    p.header16.copy_from_slice(&plain[..16]);
    if let Some(so) = find_meta(&plain, root as u64) {
        p.found = true;
        if so + 26 <= plain.len() {
            p.v06_07 = u16::from_le_bytes([plain[so + 6], plain[so + 7]]);
            p.v12_13 = u16::from_le_bytes([plain[so + 12], plain[so + 13]]);
            p.v14 = plain[so + 14];
        }
        // Per C.34: row_count typically lives a few bytes after the d5 0b.
        // Try +34..+36 as u16 LE.
        if so + 36 <= plain.len() {
            p.row_count = u16::from_le_bytes([plain[so + 34], plain[so + 35]]);
        }
    }
    p
}

#[derive(Default)]
struct CrossFileRow {
    per_file: BTreeMap<String, PageProfile>,
    root_pages: BTreeMap<String, u32>,
}

fn process_file(
    path: &str,
) -> anyhow::Result<(BTreeMap<String, PageProfile>, BTreeMap<String, u32>)> {
    let store = PageStore::open(path)?;
    let model = ApModel::learn(&store);
    let entries: Vec<SysTableEntry> = iter_systable_entries(&store, &model).collect();
    let mut profiles = BTreeMap::new();
    let mut roots = BTreeMap::new();
    for e in &entries {
        if let Some(root) = e.data_root_page {
            // Only profile E-page roots; A-page roots are dbspace-wide.
            if let Ok(page) = store.page(root as u64) {
                if page.trailer().page_type() == PageType::Extent {
                    let p = profile_root(&store, &model, root);
                    profiles.insert(e.name.clone(), p);
                    roots.insert(e.name.clone(), root);
                }
            }
        }
    }
    Ok((profiles, roots))
}

fn main() -> anyhow::Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        anyhow::bail!("usage: probe_corpus <file1.qbw> [file2.qbw ...]");
    }

    let mut data: BTreeMap<String, CrossFileRow> = BTreeMap::new();
    let mut file_stats: Vec<(String, usize, usize)> = Vec::new();

    for path in &files {
        let label = Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        eprintln!("=== processing {} ===", label);
        let (profiles, roots) = process_file(path)?;
        let total = profiles.len();
        let with_meta = profiles.values().filter(|p| p.found).count();
        file_stats.push((label.clone(), total, with_meta));
        for (name, prof) in profiles {
            let row = data.entry(name.clone()).or_default();
            row.per_file.insert(label.clone(), prof);
            if let Some(&r) = roots.get(&name) {
                row.root_pages.insert(label.clone(), r);
            }
        }
    }

    println!("\n## File summary");
    for (l, t, w) in &file_stats {
        println!(
            "  {:<55} user_tables_with_e_root={:>4} meta_found={:>4}",
            l, t, w
        );
    }

    // Tables present in ALL files.
    let n_files = file_stats.len();
    let labels: Vec<String> = file_stats.iter().map(|x| x.0.clone()).collect();
    let common: Vec<&String> = data
        .iter()
        .filter(|(_, r)| r.per_file.len() == n_files)
        .map(|(n, _)| n)
        .collect();
    println!(
        "\n## Common tables (in all {} files): {}",
        n_files,
        common.len()
    );

    // Field stability analysis: for each common table, check whether
    // v06_07, v12_13, v14, row_count, header16 are stable across files.
    let mut stable_v06_07 = 0;
    let mut stable_v12_13 = 0;
    let mut stable_v14 = 0;
    let mut stable_header16 = 0;
    let mut stable_header4 = 0; // first 4 bytes
    let mut tables_meta_all = 0;

    for name in &common {
        let row = &data[*name];
        let profs: Vec<&PageProfile> = labels.iter().map(|l| &row.per_file[l]).collect();
        if !profs.iter().all(|p| p.found) {
            continue;
        }
        tables_meta_all += 1;
        let v06: Vec<u16> = profs.iter().map(|p| p.v06_07).collect();
        let v12: Vec<u16> = profs.iter().map(|p| p.v12_13).collect();
        let v14: Vec<u8> = profs.iter().map(|p| p.v14).collect();
        let h16: Vec<[u8; 16]> = profs.iter().map(|p| p.header16).collect();
        let h4: Vec<[u8; 4]> = profs
            .iter()
            .map(|p| [p.header16[0], p.header16[1], p.header16[2], p.header16[3]])
            .collect();
        if v06.iter().all(|&v| v == v06[0]) {
            stable_v06_07 += 1;
        }
        if v12.iter().all(|&v| v == v12[0]) {
            stable_v12_13 += 1;
        }
        if v14.iter().all(|&v| v == v14[0]) {
            stable_v14 += 1;
        }
        if h16.iter().all(|h| h == &h16[0]) {
            stable_header16 += 1;
        }
        if h4.iter().all(|h| h == &h4[0]) {
            stable_header4 += 1;
        }
    }

    println!(
        "\n## Cross-file stability ({} common tables with metadata in all files)",
        tables_meta_all
    );
    println!(
        "  v06_07 stable per-table:   {}/{}",
        stable_v06_07, tables_meta_all
    );
    println!(
        "  v12_13 stable per-table:   {}/{}",
        stable_v12_13, tables_meta_all
    );
    println!(
        "  v14    stable per-table:   {}/{}",
        stable_v14, tables_meta_all
    );
    println!(
        "  header[0..4] stable:       {}/{}",
        stable_header4, tables_meta_all
    );
    println!(
        "  header[0..16] stable:      {}/{}",
        stable_header16, tables_meta_all
    );

    // Detail dump: per-table v06_07/v12_13 across files for the first 12
    // common tables that have meta in all files (sanity check).
    println!("\n## Sample per-table v06_07 / v12_13 across files");
    let mut shown = 0;
    for name in &common {
        let row = &data[*name];
        if !row.per_file.values().all(|p| p.found) {
            continue;
        }
        if shown >= 12 {
            break;
        }
        let v06s: Vec<String> = labels
            .iter()
            .map(|l| format!("{:04x}", row.per_file[l].v06_07))
            .collect();
        let v12s: Vec<String> = labels
            .iter()
            .map(|l| format!("{:04x}", row.per_file[l].v12_13))
            .collect();
        println!(
            "  {:<44} v06=[{}]  v12=[{}]",
            name,
            v06s.join(" "),
            v12s.join(" ")
        );
        shown += 1;
    }

    // File-universal constants: scan the first 64 bytes of every E-page root
    // and find byte positions whose value is the SAME across all files for
    // ALL common tables (i.e. position p is a format constant).
    println!("\n## Format-universal byte positions in header[0..64]");
    let mut universal_positions: Vec<(usize, u8)> = Vec::new();
    'pos: for pos in 0..16 {
        let first = {
            let mut bytes: Vec<u8> = Vec::new();
            for name in &common {
                for l in &labels {
                    bytes.push(data[*name].per_file[l].header16[pos]);
                }
            }
            bytes
        };
        if first.is_empty() {
            continue;
        }
        let v0 = first[0];
        for b in &first {
            if *b != v0 {
                continue 'pos;
            }
        }
        universal_positions.push((pos, v0));
    }
    for (pos, v) in &universal_positions {
        println!("  header[{:>2}] = 0x{:02x} (universal)", pos, v);
    }

    Ok(())
}
