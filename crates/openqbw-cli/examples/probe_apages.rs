//! WP-3D: A-page allocation-map reverse engineering.
//!
//! Goal: test whether A-pages encode per-table page->table ownership, so that
//! walking the A-page chain reachable from a table's SYSTABLE.data_root yields
//! that table's E-pages.
//!
//! Per C.37, A-pages contain:
//!   - Repeating 28-byte (or VA_INT) records at the start of the page body
//!     ("B-tree entries encoding E-page reference, free-space info").
//!   - A metadata block beginning at SELF_REF, with a `d5 0b` anchor, an
//!     optional `next_pn` u32 LE pointer (when flag=1), and rank fields.
//!
//! This probe:
//!   1. Enumerates every A-page in the file.
//!   2. Decodes each via `recover_bv_qb_data` (anchor `[rc,0,d5,0b]` works
//!      for both E-pages and A-pages per C.37).
//!   3. Locates the metadata block, extracts `next_pn` and rank.
//!   4. Parses the body region BEFORE the metadata block as repeating fixed-
//!      width records; for each candidate stride in {16, 20, 24, 28, 32},
//!      scans for u32 LE values that look like in-file page numbers (i.e.
//!      <= page_count and where the target page is an E-page).
//!   5. Cross-references: for each SYSTABLE entry whose data_root is an
//!      A-page, walks the chain via next_pn, collects all candidate E-page
//!      references, and reports how cleanly the collected set matches the
//!      ground-truth "E-pages between data_root and last_page" envelope.

use openqbw::{
    deobfuscate_with_bv, iter_systable_entries, recover_bv_qb_data,
    SysTableEntry,
};
use opensqlany::{ApModel, PageStore, PageType};
use std::collections::{BTreeMap, BTreeSet};

const D5_0B: [u8; 2] = [0xD5, 0x0B];
const SCAN_MAX: usize = 0xFF0;

/// Decode an A-page. SA17 ApModel (block-bv learner) is primary, since
/// `recover_bv_qb_data` assumes E-page step semantics and produces wrong
/// steps for A-pages where `plain[0] != 0`.
fn decode_apage(
    pn: u64,
    raw: &[u8],
    model: &ApModel,
    store: &PageStore,
) -> (Vec<u8>, &'static str) {
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        return (deobfuscate_with_bv(raw, pn, bv), "qb_anchor");
    }
    (model.deobfuscate_with_store(raw, pn, store), "sa17_model")
}

/// Find the SELF_REF/d5-0b metadata block. Returns the SELF_REF offset.
fn find_metadata(plain: &[u8], pn: u64) -> Option<usize> {
    let scan_end = plain.len().min(SCAN_MAX);
    if scan_end < 26 {
        return None;
    }
    let target = (pn as u32).to_le_bytes();
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

/// Read the metadata block. Returns (next_pn, flag, rank_lo).
/// Layout per C.37 (relative to SELF_REF offset `so`):
///   so+24..+26 : d5 0b
///   so+26      : flag (1 = has next-pointer)
///   so+27..+30 : 00 00 00
///   so+30..+34 : next_pn u32 LE (only meaningful if flag=1)
///   so+34..+36 : cc 0e or similar
///   so+36..+38 : cc 0f or similar
///   so+38      : seq
///   so+39..+41 : rank_lo u16 LE? (one interpretation)
fn read_metadata(plain: &[u8], so: usize) -> Option<(Option<u32>, u8, u8)> {
    if so + 40 > plain.len() {
        return None;
    }
    let flag = plain[so + 26];
    let next_pn = if flag == 1 {
        Some(u32::from_le_bytes([
            plain[so + 30],
            plain[so + 31],
            plain[so + 32],
            plain[so + 33],
        ]))
    } else {
        None
    };
    let seq = plain[so + 38];
    Some((next_pn, flag, seq))
}

/// Scan the body BEFORE the metadata block for u32 LE values that look
/// like in-file page references to E-pages.
fn scan_page_refs(
    plain: &[u8],
    end: usize,
    e_pages: &BTreeSet<u32>,
    stride: usize,
    offset: usize,
) -> Vec<u32> {
    let mut refs = Vec::new();
    if stride == 0 {
        return refs;
    }
    let mut i = offset;
    while i + 4 <= end {
        let v = u32::from_le_bytes([plain[i], plain[i + 1], plain[i + 2], plain[i + 3]]);
        if e_pages.contains(&v) {
            refs.push(v);
        }
        i += stride;
    }
    refs
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: probe_apages <file.qbw>"))?;
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let _ = &model;
    let n = store.page_count();

    // Enumerate page types.
    let mut a_pages: Vec<u64> = Vec::new();
    let mut e_pages: BTreeSet<u32> = BTreeSet::new();
    for pn in 1..n {
        if let Ok(p) = store.page(pn) {
            match p.trailer().page_type() {
                PageType::Alloc => a_pages.push(pn),
                PageType::Extent => {
                    e_pages.insert(pn as u32);
                }
                _ => {}
            }
        }
    }

    println!(
        "# {}  pages={} A-pages={} E-pages={}",
        path,
        n,
        a_pages.len(),
        e_pages.len()
    );

    // SYSTABLE ground truth.
    let entries: Vec<SysTableEntry> = iter_systable_entries(&store, &model).collect();
    let entries_with_root: Vec<&SysTableEntry> = entries
        .iter()
        .filter(|e| e.data_root_page.is_some())
        .collect();
    println!(
        "# SYSTABLE entries: {} (with data_root: {})",
        entries.len(),
        entries_with_root.len()
    );

    // Decode every A-page and pull metadata.
    let mut a_meta: BTreeMap<u32, (Option<u32>, u8, u8)> = BTreeMap::new();
    let mut a_plain: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut a_meta_off: BTreeMap<u32, usize> = BTreeMap::new();
    let mut decode_methods: BTreeMap<&str, usize> = BTreeMap::new();
    let mut no_meta = 0usize;
    for &pn in &a_pages {
        let raw = store.page(pn)?;
        let raw_bytes = raw.bytes();
        let (plain, method) = decode_apage(pn, raw_bytes, &model, &store);
        *decode_methods.entry(method).or_default() += 1;
        if let Some(so) = find_metadata(&plain, pn) {
            if let Some(meta) = read_metadata(&plain, so) {
                a_meta.insert(pn as u32, meta);
                a_meta_off.insert(pn as u32, so);
                a_plain.insert(pn as u32, plain);
                continue;
            }
        }
        no_meta += 1;
    }
    println!(
        "# decode methods: {:?}  meta_found={}  no_meta={}",
        decode_methods,
        a_meta.len(),
        no_meta
    );

    // Chain stats.
    let with_next = a_meta.values().filter(|(n, _, _)| n.is_some()).count();
    println!(
        "# A-pages with next_pn (flag=1): {}/{}",
        with_next,
        a_meta.len()
    );

    // Distinct seq values (rank/seq byte at +38).
    let mut seq_hist: BTreeMap<u8, usize> = BTreeMap::new();
    for (_, _, s) in a_meta.values() {
        *seq_hist.entry(*s).or_default() += 1;
    }
    println!("# distinct seq bytes: {} sample: {:?}",
             seq_hist.len(),
             seq_hist.iter().take(10).collect::<Vec<_>>());

    // Build reverse chain: predecessor map. For each A-page B whose meta says
    // next_pn = C, record predecessor[C] = B. Chain heads are A-pages with no
    // predecessor.
    let mut predecessor: BTreeMap<u32, u32> = BTreeMap::new();
    for (&pn, (next, _, _)) in &a_meta {
        if let Some(np) = next {
            predecessor.insert(*np, pn);
        }
    }
    let chain_heads: Vec<u32> = a_meta
        .keys()
        .copied()
        .filter(|p| !predecessor.contains_key(p))
        .collect();
    println!("# chain heads: {}", chain_heads.len());

    // Walk chains and compute length distribution.
    let mut chain_lens: Vec<usize> = Vec::new();
    let mut chain_owner_map: BTreeMap<u32, u32> = BTreeMap::new(); // member -> head
    for &head in &chain_heads {
        let mut cur = head;
        let mut len = 0usize;
        loop {
            if chain_owner_map.contains_key(&cur) {
                break; // cycle protection
            }
            chain_owner_map.insert(cur, head);
            len += 1;
            match a_meta.get(&cur).and_then(|(n, _, _)| *n) {
                Some(np) if a_meta.contains_key(&np) => cur = np,
                _ => break,
            }
        }
        chain_lens.push(len);
    }
    chain_lens.sort_unstable();
    let chain_total: usize = chain_lens.iter().sum();
    println!(
        "# chains: count={} total_members={} max_len={} median_len={}",
        chain_lens.len(),
        chain_total,
        chain_lens.last().copied().unwrap_or(0),
        chain_lens.get(chain_lens.len() / 2).copied().unwrap_or(0),
    );

    // For each SYSTABLE entry whose data_root is an A-page, follow the chain
    // and harvest page-refs from the body.
    println!("\n## Per-table A-page chain analysis (data_root is A-page)");
    println!(
        "{:>6}  {:<42}  {:>6}  {:>6}  {:>6}  {:>5}  {:>5}  {:>5}  best_stride/off",
        "tid", "name", "root", "last", "chain", "S16", "S24", "S28"
    );

    let mut a_root_count = 0usize;
    for e in &entries_with_root {
        let root = match e.data_root_page {
            Some(r) => r,
            None => continue,
        };
        if !a_meta.contains_key(&root) {
            continue;
        }
        a_root_count += 1;

        // Walk the chain from root (following next_pn).
        let mut chain: Vec<u32> = Vec::new();
        let mut cur = root;
        loop {
            if chain.contains(&cur) {
                break;
            }
            chain.push(cur);
            match a_meta.get(&cur).and_then(|(n, _, _)| *n) {
                Some(np) if a_meta.contains_key(&np) => cur = np,
                _ => break,
            }
        }

        // For each stride in {16, 20, 24, 28}, count distinct E-page refs
        // harvested from the body of all chain members (region 0 ..
        // metadata_offset).
        let strides = [16usize, 20, 24, 28];
        let mut stride_counts = [0usize; 4];
        let mut best_in_envelope = 0usize;
        let mut best_total = 0usize;
        let mut best_stride = 0usize;
        let mut best_offset = 0usize;
        let last = e.last_page.unwrap_or(root).max(root);
        let envelope: BTreeSet<u32> = e_pages
            .range(root..=last)
            .copied()
            .collect();
        for (si, &stride) in strides.iter().enumerate() {
            // Try offsets 0..stride
            for offset in 0..stride.min(8) {
                let mut all_refs: BTreeSet<u32> = BTreeSet::new();
                for &m in &chain {
                    let plain = a_plain.get(&m).expect("decoded");
                    let end = a_meta_off[&m];
                    let refs = scan_page_refs(plain, end, &e_pages, stride, offset);
                    all_refs.extend(refs);
                }
                let in_env = all_refs.intersection(&envelope).count();
                if in_env > best_in_envelope {
                    best_in_envelope = in_env;
                    best_total = all_refs.len();
                    best_stride = stride;
                    best_offset = offset;
                }
                if offset == 0 {
                    stride_counts[si] = all_refs.len();
                }
            }
        }

        println!(
            "{:>6}  {:<42}  {:>6}  {:>6}  {:>6}  {:>5}  {:>5}  {:>5}  s={}@{} env={}/total={} env_size={}",
            e.table_id,
            e.name,
            root,
            last,
            chain.len(),
            stride_counts[0],
            stride_counts[2],
            stride_counts[3],
            best_stride,
            best_offset,
            best_in_envelope,
            best_total,
            envelope.len(),
        );
    }

    println!("\n# SYSTABLE entries with A-page data_root: {}", a_root_count);

    Ok(())
}
