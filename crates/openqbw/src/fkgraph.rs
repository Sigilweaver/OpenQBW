//! Heuristic foreign-key graph (Phase 6, WP-6B).
//!
//! SA17 does not surface explicit FK declarations in any easily-parsed
//! row anchor we have recovered so far. As a stopgap, this module
//! infers candidate edges from column names alone:
//!
//! * any column whose name matches `^(.+)_id(_h)?$` is treated as a
//!   reference to a table whose name contains the captured stem;
//! * the resolver scores candidates by (a) exact stem match, (b) stem
//!   appearing as a suffix of the candidate table name, (c) stem
//!   appearing as a substring, choosing the highest-scoring table.
//!
//! Results are heuristic; an unresolved edge is still surfaced with
//! `target_table = None` so callers can audit it.

use std::collections::BTreeMap;

use opensqlany::{ApModel, PageStore};

use crate::{SysColumn, SysTableEntry, iter_syscolumns};

/// One inferred foreign-key edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FkEdge {
    /// Owning table of the referring column.
    pub source_table: String,
    /// Referring column name.
    pub source_column: String,
    /// Column id within the source table.
    pub source_column_id: u32,
    /// Captured stem (the part of `source_column` before `_id`/`_id_h`).
    pub stem: String,
    /// Best-scoring target table name, if any.
    pub target_table: Option<String>,
    /// Score of the best match (higher is better, 0 means no match).
    pub score: u32,
}

fn strip_id_suffix(name: &str) -> Option<&str> {
    if let Some(s) = name.strip_suffix("_id_h") {
        return Some(s);
    }
    if let Some(s) = name.strip_suffix("_id") {
        return Some(s);
    }
    None
}

/// Score a candidate target table for a given stem. Higher is better.
fn match_score(stem: &str, table: &str) -> u32 {
    let stem_l = stem.to_ascii_lowercase();
    let table_l = table.to_ascii_lowercase();
    if table_l == stem_l {
        return 1000;
    }
    // Strip common SA prefixes from tables to allow `abmc_invoice` to
    // match stem `invoice`.
    let stripped = table_l
        .strip_prefix("abmc_")
        .or_else(|| table_l.strip_prefix("es_"))
        .or_else(|| table_l.strip_prefix("i_"))
        .or_else(|| table_l.strip_prefix("v_"))
        .or_else(|| table_l.strip_prefix("mv_"))
        .or_else(|| table_l.strip_prefix("ix_"))
        .unwrap_or(&table_l);
    if stripped == stem_l {
        return 900;
    }
    if stripped.ends_with(&format!("_{}", stem_l)) {
        return 700;
    }
    if stripped.starts_with(&format!("{}_", stem_l)) {
        return 600;
    }
    if stripped.contains(&stem_l) {
        return 300;
    }
    0
}

/// Build a heuristic FK edge list from the parsed SYSTABLE + SYSCOLUMN
/// catalogs. The list is sorted by (source_table, source_column_id).
pub fn build(store: &PageStore, model: &ApModel) -> Vec<FkEdge> {
    let tables: Vec<SysTableEntry> = crate::collect_unique(store, model);
    // Map data_root_page -> name so we can name each SYSCOLUMN owner.
    let mut name_by_root: BTreeMap<u32, String> = BTreeMap::new();
    for t in &tables {
        if let Some(root) = t.data_root_page {
            name_by_root.entry(root).or_insert(t.name.clone());
        }
    }
    let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();

    let mut edges: Vec<FkEdge> = Vec::new();
    for c in iter_syscolumns(store, model) {
        let Some(stem) = strip_id_suffix(&c.name) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        let Some(src) = name_by_root.get(&c.owner_object_id) else {
            continue;
        };
        let (best, score) = pick_best(stem, &table_names);
        edges.push(FkEdge {
            source_table: src.clone(),
            source_column: c.name.clone(),
            source_column_id: c.column_id,
            stem: stem.to_owned(),
            target_table: best,
            score,
        });
    }
    edges.sort_by(|a, b| {
        a.source_table
            .cmp(&b.source_table)
            .then(a.source_column_id.cmp(&b.source_column_id))
    });
    edges.dedup_by(|a, b| {
        a.source_table == b.source_table
            && a.source_column == b.source_column
            && a.source_column_id == b.source_column_id
    });
    edges
}

fn pick_best(stem: &str, tables: &[String]) -> (Option<String>, u32) {
    let mut best: Option<&String> = None;
    let mut best_score: u32 = 0;
    for t in tables {
        let s = match_score(stem, t);
        if s > best_score {
            best_score = s;
            best = Some(t);
        }
    }
    if best_score == 0 {
        (None, 0)
    } else {
        (best.cloned(), best_score)
    }
}

/// Summary of edge resolution rates.
#[derive(Debug, Clone, Copy)]
pub struct FkGraphStats {
    /// Total inferred edges.
    pub edges: usize,
    /// Edges with a resolved target table.
    pub resolved: usize,
    /// Edges that resolved with exact stem match (score >= 900).
    pub strong: usize,
}

/// Compute simple counts over an edge set.
pub fn stats(edges: &[FkEdge]) -> FkGraphStats {
    FkGraphStats {
        edges: edges.len(),
        resolved: edges.iter().filter(|e| e.target_table.is_some()).count(),
        strong: edges.iter().filter(|e| e.score >= 900).count(),
    }
}

#[allow(dead_code)]
fn _unused_marker(_c: &SysColumn) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_id_h_then_id() {
        assert_eq!(strip_id_suffix("account_id"), Some("account"));
        assert_eq!(strip_id_suffix("contact_id_h"), Some("contact"));
        assert_eq!(strip_id_suffix("name"), None);
        assert_eq!(strip_id_suffix("_id"), Some(""));
    }

    #[test]
    fn exact_match_beats_substring() {
        assert!(
            match_score("invoice", "abmc_invoice")
                > match_score("invoice", "abmc_invoice_lineitem")
        );
    }

    #[test]
    fn no_match_returns_zero() {
        assert_eq!(match_score("zzzz", "abmc_invoice"), 0);
    }

    #[test]
    fn pick_best_returns_best_table() {
        let tables = vec![
            "abmc_invoice".to_string(),
            "abmc_invoice_lineitem".to_string(),
            "abmc_check".to_string(),
        ];
        let (best, score) = pick_best("invoice", &tables);
        assert_eq!(best.as_deref(), Some("abmc_invoice"));
        assert!(score >= 900);
    }
}
