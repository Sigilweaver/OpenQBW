//! WP-3A: Page-side attribution probe.
//!
//! For every E-page in the file, decode it with `recover_bv_qb_data`, find
//! the C.34 metadata block at the SELF_REF offset, and extract the two
//! "variable" fields at +06..+07 and +12..+13. Cluster pages by these
//! values and compare cluster cardinality with the SYSTABLE user-table
//! count. Cross-reference each cluster with SYSTABLE.data_root_page ground
//! truth.
//!
//! Run:
//!   cargo run --release -p openqbw-cli --example probe_attribution -- <file.qbw>

use openqbw::{
    collect_unique, deobfuscate_with_bv, iter_systable_entries, oracle_bv_e_page,
    recover_bv_qb_data,
};
use opensqlany::{ApModel, PageStore, PageType};
use std::collections::BTreeMap;

const SELF_REF_SCAN_MAX: usize = 0x300;
const F32_ONE: [u8; 4] = [0x00, 0x00, 0x80, 0x3F];
const D5_0B: [u8; 2] = [0xD5, 0x0B];

/// Locate the C.34 metadata block. Strategy A: find SELF_REF first.
fn find_metadata_by_selfref(plain: &[u8], pn: u64) -> Option<usize> {
    let target = (pn as u32).to_le_bytes();
    let scan_end = plain.len().min(SELF_REF_SCAN_MAX);
    if scan_end < 12 {
        return None;
    }
    for off in 0..scan_end.saturating_sub(12) {
        if plain[off..off + 4] == target
            && plain[off + 4] == 0
            && plain[off + 5] == 0
            && plain[off + 8..off + 12] == F32_ONE
        {
            return Some(off);
        }
    }
    None
}

/// Strategy B: find d5 0b anchor and walk back 24 bytes for SELF_REF.
fn find_metadata_by_anchor(plain: &[u8], pn: u64) -> Option<usize> {
    let scan_end = plain.len().min(0xFF0);
    if scan_end < 26 {
        return None;
    }
    let target = (pn as u32).to_le_bytes();
    let mut i = 24usize;
    while i < scan_end - 1 {
        if plain[i..i + 2] == D5_0B {
            // SELF_REF expected at i - 24
            let so = i - 24;
            if plain[so..so + 4] == target {
                return Some(so);
            }
        }
        i += 1;
    }
    None
}

fn find_metadata_offset(plain: &[u8], pn: u64) -> Option<usize> {
    find_metadata_by_selfref(plain, pn).or_else(|| find_metadata_by_anchor(plain, pn))
}

#[derive(Debug, Clone, Copy)]
struct Meta {
    v06_07: u16,
    v12_13: u16,
    v14: u8,
    row_count: u8,
    magic_ok: bool,
}

fn read_metadata(plain: &[u8], pn: u64) -> Option<Meta> {
    let off = find_metadata_offset(plain, pn)?;
    if off + 26 > plain.len() {
        return None;
    }
    let blk = &plain[off..off + 26];
    Some(Meta {
        v06_07: u16::from_le_bytes([blk[6], blk[7]]),
        v12_13: u16::from_le_bytes([blk[12], blk[13]]),
        v14: blk[14],
        row_count: blk[22],
        magic_ok: blk[24] == 0xD5 && blk[25] == 0x0B,
    })
}

fn decode_page(
    raw: &[u8],
    pn: u64,
    model: &ApModel,
    store: &PageStore,
) -> (Vec<u8>, &'static str) {
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        (deobfuscate_with_bv(raw, pn, bv), "qb_anchor")
    } else {
        let bv = oracle_bv_e_page(pn, raw);
        let cand = deobfuscate_with_bv(raw, pn, bv);
        if cand[0] == 0 {
            (cand, "oracle")
        } else {
            (model.deobfuscate_with_store(raw, pn, store), "generic")
        }
    }
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: probe_attribution <file.qbw>"))?;
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let n = store.page_count();
    println!("# {}  ({} pages)", path, n);

    // Build SYSTABLE ground-truth: data_root_page -> (tid, name)
    let _ = iter_systable_entries; // silence unused warning if any
    let unique = collect_unique(&store, &model);
    let mut root_to_table: BTreeMap<u32, (u32, String)> = BTreeMap::new();
    for e in &unique {
        if let Some(root) = e.data_root_page {
            if root > 0 && (root as u64) < n {
                root_to_table.insert(root, (e.table_id, e.name.clone()));
            }
        }
    }
    println!("# SYSTABLE entries: {}", unique.len());
    println!("# Ground-truth data_root_page E-pages: {}", root_to_table.len());

    // Decode every E-page, extract metadata
    let mut e_pages = 0u64;
    let mut decode_method = std::collections::BTreeMap::<&str, u64>::new();
    let mut metadata_ok = 0u64;
    let mut metadata_fail = 0u64;
    let mut page_to_meta: BTreeMap<u64, Meta> = BTreeMap::new();

    for pn in 1..n {
        let page = store.page(pn)?;
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        e_pages += 1;
        let raw = page.bytes();
        let (plain, method) = decode_page(raw, pn, &model, &store);
        *decode_method.entry(method).or_default() += 1;
        match read_metadata(&plain, pn) {
            Some(m) if m.magic_ok => {
                metadata_ok += 1;
                page_to_meta.insert(pn, m);
            }
            _ => {
                metadata_fail += 1;
            }
        }
    }
    println!("# E-pages total: {}", e_pages);
    println!("# decode methods: {:?}", decode_method);
    println!("# E-pages with valid metadata (d5 0b ok): {}", metadata_ok);
    println!("# E-pages without valid metadata: {}", metadata_fail);

    // Cluster by (v06_07, v12_13)
    let mut cluster_v06: BTreeMap<u16, Vec<u64>> = BTreeMap::new();
    let mut cluster_v12: BTreeMap<u16, Vec<u64>> = BTreeMap::new();
    let mut cluster_both: BTreeMap<(u16, u16), Vec<u64>> = BTreeMap::new();
    for (&pn, m) in &page_to_meta {
        cluster_v06.entry(m.v06_07).or_default().push(pn);
        cluster_v12.entry(m.v12_13).or_default().push(pn);
        cluster_both.entry((m.v06_07, m.v12_13)).or_default().push(pn);
    }
    println!();
    println!("## Cluster cardinality");
    println!("  distinct v06_07: {}", cluster_v06.len());
    println!("  distinct v12_13: {}", cluster_v12.len());
    println!("  distinct (v06_07, v12_13): {}", cluster_both.len());

    // For each ground-truth root, dump its metadata and the cluster size
    println!();
    println!("## Ground-truth roots: meta + cluster sizes");
    println!(
        "  {:>5}  {:<40}  {:>5}  {:>6}  {:>6}  {:>4}  {:>6}  {:>6}",
        "tid", "name", "pn", "v06_07", "v12_13", "v14", "|v06|", "|v12|"
    );
    let mut shown = 0;
    for (&root, (tid, name)) in &root_to_table {
        if name.starts_with("SYS") || name.starts_with("ISYS") {
            continue;
        }
        match page_to_meta.get(&(root as u64)) {
            Some(m) => {
                let c06 = cluster_v06.get(&m.v06_07).map(|v| v.len()).unwrap_or(0);
                let c12 = cluster_v12.get(&m.v12_13).map(|v| v.len()).unwrap_or(0);
                println!(
                    "  {:>5}  {:<40}  {:>5}  {:>#06x}  {:>#06x}  {:>#04x}  {:>6}  {:>6}",
                    tid, name, root, m.v06_07, m.v12_13, m.v14, c06, c12
                );
            }
            None => {
                println!(
                    "  {:>5}  {:<40}  {:>5}  (no metadata at root)",
                    tid, name, root
                );
            }
        }
        shown += 1;
        if shown >= 60 {
            break;
        }
    }

    // Show ALL v12_13 clusters with owners
    println!();
    println!("## All v12_13 clusters");
    let mut v12_sizes: Vec<(u16, usize)> =
        cluster_v12.iter().map(|(k, v)| (*k, v.len())).collect();
    v12_sizes.sort_by_key(|&(_, sz)| std::cmp::Reverse(sz));
    for (v, sz) in v12_sizes {
        let pages = &cluster_v12[&v];
        let mut owners: BTreeMap<u32, &str> = BTreeMap::new();
        for &p in pages {
            if let Some((tid, name)) = root_to_table.get(&(p as u32)) {
                owners.insert(*tid, name.as_str());
            }
        }
        let owners_str: Vec<String> =
            owners.iter().map(|(t, n)| format!("{}={}", t, n)).collect();
        println!(
            "  v12_13={:#06x}  pages={:>5}  roots_owned: {:?}",
            v, sz, owners_str
        );
    }

    // Now investigate the 7517 E-pages that DON'T match the C.34 pattern.
    // Dump first-32-byte signatures (after self-ref location, if findable
    // by another heuristic). For now: cluster by bytes [4..8] of plaintext.
    println!();
    println!("## E-pages without C.34 metadata - first-32 plaintext byte distribution");
    let mut sig_cluster: BTreeMap<[u8; 8], u64> = BTreeMap::new();
    let mut no_meta_sample: Vec<(u64, Vec<u8>)> = Vec::new();
    for pn in 1..n {
        let page = store.page(pn)?;
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        if page_to_meta.contains_key(&pn) {
            continue;
        }
        let raw = page.bytes();
        let (plain, _) = decode_page(raw, pn, &model, &store);
        // Use bytes [0..8] as signature for clustering page-header opening
        let mut sig = [0u8; 8];
        sig.copy_from_slice(&plain[0..8]);
        *sig_cluster.entry(sig).or_default() += 1;
        if no_meta_sample.len() < 6 && root_to_table.contains_key(&(pn as u32)) {
            no_meta_sample.push((pn, plain[..64].to_vec()));
        }
    }
    println!("  distinct [0..8] signatures: {}", sig_cluster.len());
    let mut sig_sorted: Vec<_> = sig_cluster.iter().collect();
    sig_sorted.sort_by_key(|&(_, c)| std::cmp::Reverse(*c));
    println!("  top 10:");
    for (sig, count) in sig_sorted.iter().take(10) {
        println!("    {:02x?}  count={}", sig, count);
    }

    // Sample 6 known-root pages without C.34 metadata - dump their first 64 bytes
    println!();
    println!("## Sample known-root pages without C.34 metadata (first 64 bytes plaintext)");
    for (pn, head) in &no_meta_sample {
        let (tid, name) = &root_to_table[&(*pn as u32)];
        println!("  page {} (tid={} {}):", pn, tid, name);
        for chunk_off in (0..head.len()).step_by(16) {
            let chunk = &head[chunk_off..(chunk_off + 16).min(head.len())];
            let hex: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                .collect();
            println!("    {:04x}: {:<47}  {}", chunk_off, hex.join(" "), ascii);
        }
    }

    Ok(())
}
