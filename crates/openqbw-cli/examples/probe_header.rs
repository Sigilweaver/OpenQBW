use openqbw::{PageAttribution, deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};
use opensqlany::{ApModel, Page, PageStore, PageType, SlottedPage};
use std::collections::{BTreeMap, HashSet};

fn is_base62(s: &[u8]) -> bool {
    s.iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'))
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let attr = PageAttribution::build(&store, &model);
    let n = store.page_count();

    let mut linear_ids: HashSet<String> = HashSet::new();
    let mut slotted_ids: HashSet<String> = HashSet::new();
    let mut slotted_ids_first16: HashSet<String> = HashSet::new();
    let mut slotted_per_table: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let mut pages_with_dir = 0u64;
    let mut pages_e = 0u64;

    for pn in 1..n {
        let page = store.page(pn)?;
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        pages_e += 1;
        let raw = page.bytes();
        let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
            deobfuscate_with_bv(raw, pn, bv)
        } else {
            let bv = oracle_bv_e_page(pn, raw);
            let candidate = deobfuscate_with_bv(raw, pn, bv);
            if candidate[0] == 0 {
                candidate
            } else {
                model.deobfuscate_with_store(raw, pn, &store)
            }
        };
        let body = &plain[..0xFF0];
        let mut pos = 0usize;
        while pos + 19 <= body.len() {
            if body[pos] == 0x0E
                && body[pos + 1] == 0x00
                && body[pos + 2] == 0x10
                && is_base62(&body[pos + 3..pos + 19])
            {
                let id = std::str::from_utf8(&body[pos + 3..pos + 19])
                    .unwrap()
                    .to_owned();
                linear_ids.insert(id);
                pos += 19;
            } else {
                pos += 1;
            }
        }
        let p = Page::from_bytes(pn, &plain);
        let sp = SlottedPage::parse(p);
        if let Some(_d) = &sp.directory {
            pages_with_dir += 1;
            let source = attr
                .attribute(pn)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "<unattributed>".to_string());
            for (_, row) in sp.row_bytes() {
                let scan_end = row.len();
                let mut found = None;
                for i in 0..scan_end.saturating_sub(19) {
                    if row[i] == 0x0E
                        && row[i + 1] == 0x00
                        && row[i + 2] == 0x10
                        && is_base62(&row[i + 3..i + 19])
                    {
                        let id = std::str::from_utf8(&row[i + 3..i + 19]).unwrap().to_owned();
                        slotted_ids.insert(id.clone());
                        if i < 16 {
                            slotted_ids_first16.insert(id.clone());
                        }
                        if found.is_none() {
                            found = Some(id);
                        }
                        break;
                    }
                }
                if let Some(id) = found {
                    slotted_per_table
                        .entry(source.clone())
                        .or_default()
                        .insert(id);
                }
            }
        }
    }
    println!("E-pages={} with-slot-dir={}", pages_e, pages_with_dir);
    println!("Linear-scan distinct 0E0010 QB-IDs: {}", linear_ids.len());
    println!(
        "Slotted-scan distinct QB-IDs (anchor anywhere in row): {}",
        slotted_ids.len()
    );
    println!(
        "Slotted-scan distinct QB-IDs (anchor in first 16 bytes of row): {}",
        slotted_ids_first16.len()
    );
    println!("Slotted by source_table:");
    let mut v: Vec<_> = slotted_per_table.iter().collect();
    v.sort_by_key(|(_, ids)| std::cmp::Reverse(ids.len()));
    for (t, ids) in v.iter().take(20) {
        println!("  {:50} {}", t, ids.len());
    }
    Ok(())
}
