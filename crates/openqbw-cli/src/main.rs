//! `openqbw` command-line tool.
//!
//! Subcommands:
//!
//! ```text
//! openqbw export    <input.qbw> <output.db>   # multi-transaction SQLite export
//! openqbw catalog   <input.qbw>               # SYSTABLE listing
//! openqbw verify    <input.qbw>               # validation summary
//! openqbw indexes   <input.qbw>               # SYSINDEX listing + attribution audit
//! openqbw migrate   <input.qbw> --format=...  # data-liberation export (csv/sqlite/iif)
//! openqbw forensics <input.qbw>               # file-level discovery summary
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use openqbw::{
    AmountType, ContentAttribution, CrossValidation, LineItem, PageAttribution, SysIndexEntry,
    SysTableEntry, TransactionHeader, iter_lineitems_with_attribution, iter_transaction_headers,
};
use opensqlany::{ApModel, PageStore};
use rusqlite::{Connection, Transaction, params};

const PHASE5_INVOICE_TOTAL_CENTS: i64 = 39_991_479_278;
const PHASE5_INVOICE_COUNT: i64 = 13_375;

#[derive(Parser, Debug)]
#[command(
    name = "openqbw",
    version,
    about = "QuickBooks .qbw inspector and exporter"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Export transactions and line items to SQLite, attributing each
    /// line item to its source table via SYSTABLE.
    Export {
        /// Input QBW file.
        input: PathBuf,
        /// Output SQLite database (will be overwritten).
        output: PathBuf,
    },
    /// List the SYSTABLE catalog (table_id, name, root page) recovered
    /// directly from the QBW file.
    Catalog {
        /// Input QBW file.
        input: PathBuf,
        /// Show only user-looking tables (skip SYS*/ISYS*/RS_*/dbo*).
        #[arg(long)]
        user_only: bool,
    },
    /// Validate an export against known invariants (invoice regression,
    /// journal sum-to-zero, per-table coverage).
    Verify {
        /// Input QBW file.
        input: PathBuf,
        /// Optional output SQLite path. Defaults to a temp file.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Also build a content-signature attribution map and report
        /// per-page agreement against the default position-based
        /// attribution.
        #[arg(long)]
        strict_attribution: bool,
    },
    /// Print the columns of a table. SYSCOLUMN.owner_object_id is
    /// bridged to SYSTABLE.name via the SYSOBJECT catalog
    /// (Phase 6, WP-6Z.2).
    Schema {
        /// Input QBW file.
        input: PathBuf,
        /// Table name to inspect.
        table: String,
    },
    /// Print heuristic foreign-key edges inferred from column names
    /// matching `*_id` / `*_id_h` (Phase 6, WP-6B).
    Fkgraph {
        /// Input QBW file.
        input: PathBuf,
        /// Only print edges that resolved to a target table.
        #[arg(long)]
        resolved_only: bool,
    },
    /// Print a histogram of SYSCOLUMN.nulls_flag bytes with sample
    /// columns per value (Phase 6, WP-6D).
    Nulls {
        /// Input QBW file.
        input: PathBuf,
    },
    /// Validate position-based page attribution against per-table
    /// row-width bands derived from SYSCOLUMN (Phase 6, WP-6Z).
    ValidateAttribution {
        /// Input QBW file.
        input: PathBuf,
    },
    /// List the SYSINDEX catalog and cross-validate the position
    /// attribution against SYSINDEX `root_page` ground truth
    /// (Phase 6, WP-6Z.3).
    Indexes {
        /// Input QBW file.
        input: PathBuf,
        /// Show only foreign-key indexes (names starting with `fkey_`).
        #[arg(long)]
        fk_only: bool,
        /// Show only the cross-validation summary (no per-index rows).
        #[arg(long)]
        summary_only: bool,
    },
    /// Data-liberation export: emit line items and transaction
    /// headers in a portable format suitable for handing to a
    /// different accounting product or for archival.
    Migrate {
        /// Input QBW file.
        input: PathBuf,
        /// Output destination. For `csv` this is a directory and
        /// will be created if it does not exist; for `sqlite` and
        /// `iif` it is a single output file.
        #[arg(long)]
        out: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = MigrateFormat::Csv)]
        format: MigrateFormat,
    },
    /// File-level forensic discovery summary: size, page-type
    /// histogram, table inventory, and high-level signals useful
    /// for triage in audit or litigation contexts.
    Forensics {
        /// Input QBW file.
        input: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum MigrateFormat {
    /// One CSV file per output kind into a directory.
    Csv,
    /// A single SQLite database file (equivalent to `openqbw export`).
    Sqlite,
    /// Intuit Interchange Format (IIF), a tab-separated text format
    /// importable by many accounting products.
    Iif,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Export { input, output } => run_export(input, output).map(|_| ()),
        Cmd::Catalog { input, user_only } => run_catalog(input, user_only),
        Cmd::Verify {
            input,
            output,
            strict_attribution,
        } => run_verify(input, output, strict_attribution),
        Cmd::Schema { input, table } => run_schema(input, table),
        Cmd::Fkgraph {
            input,
            resolved_only,
        } => run_fkgraph(input, resolved_only),
        Cmd::Nulls { input } => run_nulls(input),
        Cmd::ValidateAttribution { input } => run_validate_attribution(input),
        Cmd::Indexes {
            input,
            fk_only,
            summary_only,
        } => run_indexes(input, fk_only, summary_only),
        Cmd::Migrate { input, out, format } => run_migrate(input, out, format),
        Cmd::Forensics { input } => run_forensics(input),
    }
}

fn run_export(input: PathBuf, output: PathBuf) -> Result<ExportStats> {
    if output.exists() {
        std::fs::remove_file(&output).with_context(|| format!("removing existing {:?}", output))?;
    }

    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);

    let mut items: Vec<LineItem> =
        iter_lineitems_with_attribution(&store, &model, &attribution).collect();
    items.sort_by_key(|li| (li.page_number, li.page_offset));

    let mut headers: Vec<TransactionHeader> =
        iter_transaction_headers(&store, &model, &attribution).collect();
    headers.sort_by_key(|h| (h.page_number, h.page_offset));

    let mut conn = Connection::open(&output).with_context(|| format!("opening {:?}", output))?;
    create_schema(&conn)?;

    {
        let tx = conn.transaction()?;
        let header_map = insert_transaction_headers(&tx, &headers)?;
        insert_synthesized_transactions(&tx, &items, &header_map)?;
        insert_transaction_line_items(&tx, &items)?;
        tx.commit()?;
    }

    let stats = collect_export_stats(&conn, &store, &items, &headers)?;
    println!("{}", stats.summary());
    Ok(stats)
}

fn run_catalog(input: PathBuf, user_only: bool) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);

    let mut entries: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
    entries.sort_by_key(|e| e.table_id);

    let total = entries.len();
    let user: Vec<&SysTableEntry> = entries
        .iter()
        .filter(|e| !is_system_name(&e.name))
        .collect();

    println!(
        "file: {}  pages={}  unique_tables={}  user_tables={}",
        input.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        store.page_count(),
        total,
        user.len(),
    );
    println!(
        "{:>6}  {:>5}  {:>6}  {:>6}  name",
        "tid", "cols", "root", "last"
    );
    let iter: Box<dyn Iterator<Item = &SysTableEntry>> = if user_only {
        Box::new(user.into_iter())
    } else {
        Box::new(entries.iter())
    };
    for e in iter {
        let cols = e
            .col_count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".into());
        let root = e
            .data_root_page
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into());
        let last = e
            .last_page
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>6}  {:>5}  {:>6}  {:>6}  {}",
            e.table_id, cols, root, last, e.name
        );
    }
    Ok(())
}

fn run_schema(input: PathBuf, table: String) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let cols = openqbw::schema_for(&store, &model, &table);
    if cols.is_empty() {
        anyhow::bail!(
            "no schema found for table {:?} (table may be unknown or its SYSTABLE \
             row lacks a recoverable data_root_page)",
            table
        );
    }
    println!("table: {}  columns: {}", table, cols.len());
    println!(
        "{:>5}  {:<32}  {:>6}  {:>5}  {:>5}",
        "id", "name", "domain", "width", "nulls"
    );
    for c in &cols {
        println!(
            "{:>5}  {:<32}  {:>6}  {:>5}  {:>5}",
            c.column_id, c.name, c.domain_char as char, c.width, c.nulls_flag
        );
    }
    Ok(())
}

fn run_fkgraph(input: PathBuf, resolved_only: bool) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let edges = openqbw::build_fk_graph(&store, &model);
    let s = openqbw::fk_graph_stats(&edges);
    println!(
        "edges={}  resolved={}  strong(>=900)={}  resolved_rate={:.1}%",
        s.edges,
        s.resolved,
        s.strong,
        if s.edges == 0 {
            0.0
        } else {
            100.0 * s.resolved as f64 / s.edges as f64
        }
    );
    println!(
        "{:<40}  {:>4}  {:<28}  {:<40}  {:>5}",
        "source_table", "col", "source_column", "target_table", "score"
    );
    for e in &edges {
        if resolved_only && e.target_table.is_none() {
            continue;
        }
        let tgt = e.target_table.clone().unwrap_or_else(|| "-".into());
        println!(
            "{:<40}  {:>4}  {:<28}  {:<40}  {:>5}",
            e.source_table, e.source_column_id, e.source_column, tgt, e.score
        );
    }
    Ok(())
}

fn run_indexes(input: PathBuf, fk_only: bool, summary_only: bool) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let entries: Vec<SysIndexEntry> = openqbw::collect_unique_sysindex(&store, &model);
    let tables: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
    let position = PageAttribution::build(&store, &model);
    let audit = CrossValidation::run(&entries, &position, &tables);

    let fk_count = entries.iter().filter(|e| e.is_foreign_key()).count();
    println!(
        "sysindex entries: {} (fk: {})  distinct (table_id,root_page) pairs: {}",
        entries.len(),
        fk_count,
        audit.distinct_roots,
    );
    print_audit_summary(&audit);

    if summary_only {
        return Ok(());
    }

    let mut name_by_tid: BTreeMap<u32, String> = BTreeMap::new();
    for t in &tables {
        name_by_tid
            .entry(t.table_id)
            .or_insert_with(|| t.name.clone());
    }
    println!();
    println!("{:>8}  {:>8}  {:<40}  index_name", "tid", "root", "owner");
    for e in &entries {
        if fk_only && !e.is_foreign_key() {
            continue;
        }
        let owner = name_by_tid
            .get(&e.table_id)
            .cloned()
            .unwrap_or_else(|| "<orphan>".into());
        println!(
            "{:>8}  {:>8}  {:<40}  {}",
            e.table_id, e.root_page, owner, e.name
        );
    }
    Ok(())
}

fn print_audit_summary(audit: &CrossValidation) {
    println!(
        "cross-validation: agree={} disagree={} missing={} orphan={}  agreement_rate={:.1}%",
        audit.agree,
        audit.disagree,
        audit.missing,
        audit.orphan_index,
        audit.agreement_rate() * 100.0,
    );
    if !audit.disagree_samples.is_empty() {
        println!("  disagreement samples (sysindex_table -> position_table @ root_page : index):");
        for (tid, sysn, posn, root, idx) in &audit.disagree_samples {
            println!(
                "    tid={:<6} {:<32} -> {:<32} @ root={:<8} : {}",
                tid, sysn, posn, root, idx
            );
        }
    }
}

fn run_validate_attribution(input: PathBuf) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let position = openqbw::PageAttribution::build(&store, &model);
    let schema = openqbw::SchemaAttribution::build(&store, &model);
    println!(
        "tables with width bands: {}  total pages: {}",
        schema.len(),
        store.page_count()
    );
    let pairs = (1..store.page_count())
        .filter_map(|pn| position.attribute(pn).map(|e| (pn, e.name.clone())));
    let stats = schema.validate_corpus(&store, &model, pairs);
    let total = stats.total().max(1);
    println!(
        "{:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "pass", "fail", "no_band", "unmeasured", "total"
    );
    println!(
        "{:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        stats.pass,
        stats.fail,
        stats.no_band,
        stats.unmeasured,
        stats.total()
    );
    println!(
        "pass rate: {:.1}%  fail rate: {:.1}%",
        100.0 * stats.pass as f64 / total as f64,
        100.0 * stats.fail as f64 / total as f64,
    );
    Ok(())
}

fn run_nulls(input: PathBuf) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let buckets = openqbw::nulls_flag_histogram(&store, &model);
    let total: usize = buckets.iter().map(|b| b.count).sum();
    println!(
        "total syscolumn rows: {}  distinct nulls_flag values: {}",
        total,
        buckets.len()
    );
    println!("{:>6}  {:>8}  sample_columns", "byte", "count");
    for b in &buckets {
        let samples = b.sample_columns.join(", ");
        println!("0x{:02x}    {:>8}  {}", b.flag, b.count, samples);
    }
    Ok(())
}

fn run_verify(input: PathBuf, output: Option<PathBuf>, strict_attribution: bool) -> Result<()> {
    let out = match output {
        Some(p) => p,
        None => std::env::temp_dir().join(format!("openqbw-verify-{}.sqlite", std::process::id())),
    };
    let stats = run_export(input.clone(), out.clone())?;
    println!();
    println!("=== verification report ===");

    let conn = Connection::open(&out)?;
    println!();
    println!("Per source_table line-item counts:");
    let mut stmt = conn.prepare(
        "SELECT source_table, COUNT(*), COALESCE(SUM(amount_cents), 0)
         FROM transaction_line_items
         GROUP BY source_table
         ORDER BY source_table",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (t, n, sum) = row?;
        println!("  {:44}  {:>8}  ${:>15.2}", t, n, sum as f64 / 100.0);
    }

    println!();
    println!("Per type transaction counts:");
    let mut stmt = conn.prepare(
        "SELECT type, COUNT(*), COALESCE(SUM(total_cents), 0)
         FROM transactions
         GROUP BY type
         ORDER BY type",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (t, n, sum) = row?;
        println!("  {:44}  {:>8}  ${:>15.2}", t, n, sum as f64 / 100.0);
    }

    println!();
    let parent_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT qb_id_parent) FROM transaction_line_items",
        [],
        |r| r.get(0),
    )?;
    let grand_total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM transaction_line_items",
        [],
        |r| r.get(0),
    )?;
    let regression_ok =
        grand_total == PHASE5_INVOICE_TOTAL_CENTS && parent_count == PHASE5_INVOICE_COUNT;
    println!(
        "Phase 5 regression (universal anchor):  parents={}/{}  grand_total=${:.2}/${:.2}  {}",
        parent_count,
        PHASE5_INVOICE_COUNT,
        grand_total as f64 / 100.0,
        PHASE5_INVOICE_TOTAL_CENTS as f64 / 100.0,
        if regression_ok { "PASS" } else { "DIFF" },
    );

    // Per-source-table parent counts (gives a sense of attribution
    // distribution; expect most parents under the dominant lineitem
    // tables on Rock Castle: abmc_invoice_inventory_lineitem,
    // abmc_general_journal_inventory_lineitem, abmc_credit_memo_inventory_lineitem).
    println!();
    println!("Top 10 source_tables by distinct parent QB-IDs:");
    let mut stmt = conn.prepare(
        "SELECT source_table, COUNT(DISTINCT qb_id_parent) as n
         FROM transaction_line_items
         GROUP BY source_table
         ORDER BY n DESC
         LIMIT 10",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (t, n) = row?;
        println!("  {:44}  {:>8}", t, n);
    }

    println!();
    println!("Journal sum-to-zero check (Phase 2.2, signed amounts, C.48 Track A):");
    // Same-type 2-line journal pair-balance: the strictest case where the
    // high-bit-of-byte-1 sign hypothesis is unambiguous. Acceptance target:
    // 100% same-type pair-balance.
    let pair_stats: (i64, i64) = conn.query_row(
        "SELECT
           COUNT(*) AS pairs,
           COALESCE(SUM(CASE WHEN signed_sum = 0 THEN 1 ELSE 0 END), 0) AS balanced
         FROM (
           SELECT qb_id_parent,
                  SUM(amount_cents_signed) AS signed_sum,
                  COUNT(*) AS n,
                  COUNT(DISTINCT amount_type) AS types
           FROM transaction_line_items
           WHERE source_table = 'abmc_general_journal_inventory_lineitem'
             AND amount_type IN (1, 2)
             AND amount_cents_signed IS NOT NULL
           GROUP BY qb_id_parent
           HAVING n = 2 AND types = 1
         )",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (pairs, balanced) = pair_stats;
    if pairs == 0 {
        println!("  no same-type 2-line journal pairs found");
    } else {
        let pct = (balanced as f64) / (pairs as f64) * 100.0;
        let ok = balanced == pairs;
        println!(
            "  same-type 2-line journal pairs: {}/{} balance ({:.1}%) {}",
            balanced,
            pairs,
            pct,
            if ok { "PASS" } else { "DIFF" },
        );
    }

    // Broader (all journals) for visibility - not an acceptance criterion
    // because 0x03 entries are not amount records (see C.48 Track A) and
    // attribution is fuzzy (see C.47).
    let all_stats: (i64, i64) = conn.query_row(
        "SELECT
           COUNT(*) AS p,
           COALESCE(SUM(CASE WHEN signed_sum = 0 THEN 1 ELSE 0 END), 0) AS bal
         FROM (
           SELECT qb_id_parent,
                  COALESCE(SUM(amount_cents_signed), 0) AS signed_sum
           FROM transaction_line_items
           WHERE source_table = 'abmc_general_journal_inventory_lineitem'
           GROUP BY qb_id_parent
         )",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (allp, allb) = all_stats;
    if allp > 0 {
        println!(
            "  all journal-attributed parents (signed sum incl. type-0x03 nulls): {}/{} ({:.1}%)",
            allb,
            allp,
            (allb as f64) / (allp as f64) * 100.0,
        );
    }
    println!("  note: type-0x03 entries are NOT signed amounts (727 distinct values across");
    println!("        11,754 occurrences in Rock Castle - see NOTES.md C.48 Track A).");

    println!();
    println!("=== SYSINDEX cross-validation (WP-6Z.3) ===");
    {
        let store = PageStore::open(&input).with_context(|| format!("re-opening {:?}", input))?;
        let model = ApModel::learn(&store);
        let entries = openqbw::collect_unique_sysindex(&store, &model);
        let tables: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
        let position = PageAttribution::build(&store, &model);
        let audit = CrossValidation::run(&entries, &position, &tables);
        let fk_count = entries.iter().filter(|e| e.is_foreign_key()).count();
        println!(
            "  sysindex entries: {} (fk: {})  distinct (tid,root) pairs: {}",
            entries.len(),
            fk_count,
            audit.distinct_roots,
        );
        print_audit_summary(&audit);
    }

    if strict_attribution {
        println!();
        println!("=== content-signature attribution (--strict-attribution) ===");
        let store = PageStore::open(&input).with_context(|| format!("re-opening {:?}", input))?;
        let model = ApModel::learn(&store);
        let content = ContentAttribution::build(&store, &model);
        let position = PageAttribution::build(&store, &model);
        println!(
            "  unique signatures: {}  ambiguous: {}  skipped roots: {}",
            content.len(),
            content.ambiguous_count(),
            content.skipped_count(),
        );

        // Collect the distinct E-page numbers that contributed at least
        // one line item, so the comparison runs over the pages we
        // actually attribute in production.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT page_number FROM transaction_line_items ORDER BY page_number",
        )?;
        let pages: Vec<u64> = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|n| n as u64)
            .collect();
        let agree = content.compare(&store, &model, &position, pages.iter().copied());
        let total = agree.total().max(1);
        println!(
            "  pages compared: {}  agree: {} ({:.2}%)  disagree: {}  only_position: {}  only_content: {}  neither: {}",
            agree.total(),
            agree.agree,
            (agree.agree as f64) * 100.0 / (total as f64),
            agree.disagree,
            agree.only_position,
            agree.only_content,
            agree.neither,
        );
    }

    println!();
    println!("Overall: {}", stats.summary());
    Ok(())
}

fn is_system_name(name: &str) -> bool {
    name.starts_with("SYS")
        || name.starts_with("ISYS")
        || name.starts_with("RS_")
        || name.starts_with("dbo")
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE transactions (
            qb_id        TEXT PRIMARY KEY,
            type         TEXT NOT NULL,
            source_table TEXT NOT NULL,
            txn_date_raw INTEGER,
            counter      INTEGER,
            line_count   INTEGER NOT NULL,
            total_cents  INTEGER NOT NULL,
            has_deferred INTEGER NOT NULL,
            source_page  INTEGER
        );

        CREATE TABLE transaction_line_items (
            qb_id_parent        TEXT NOT NULL,
            line_number         INTEGER NOT NULL,
            item_qb_id          TEXT,
            amount_type         INTEGER NOT NULL,
            amount_cents        INTEGER,
            amount_cents_signed INTEGER,
            amount_raw_hex      TEXT NOT NULL,
            txn_date_raw        INTEGER,
            counter             INTEGER,
            source_table        TEXT NOT NULL,
            page_number         INTEGER NOT NULL,
            page_offset         INTEGER NOT NULL,
            PRIMARY KEY (qb_id_parent, line_number, source_table)
        );

        CREATE INDEX idx_lineitems_page   ON transaction_line_items(page_number);
        CREATE INDEX idx_lineitems_source ON transaction_line_items(source_table);
        CREATE INDEX idx_tx_type          ON transactions(type);
        CREATE INDEX idx_tx_source        ON transactions(source_table);
        "#,
    )?;
    Ok(())
}

fn insert_transaction_headers(
    tx: &Transaction<'_>,
    headers: &[TransactionHeader],
) -> Result<HashMap<String, TransactionHeader>> {
    let mut map: HashMap<String, TransactionHeader> = HashMap::new();
    for h in headers {
        map.entry(h.qb_id.clone()).or_insert_with(|| h.clone());
    }
    let mut stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, \
          line_count, total_cents, has_deferred, source_page) \
         VALUES (?, ?, ?, ?, ?, 0, 0, 0, ?)",
    )?;
    let mut ordered: Vec<&TransactionHeader> = map.values().collect();
    ordered.sort_by_key(|h| (h.page_number, h.page_offset));
    for h in ordered {
        stmt.execute(params![
            h.qb_id,
            h.txn_type(),
            h.source_table,
            h.txn_date_raw,
            h.counter,
            h.page_number as i64,
        ])?;
    }
    Ok(map)
}

/// Insert synthesized rows for parent QB-IDs that appear in line items
/// but have no matching header record (or update aggregates for those
/// that do). Synthesized rows get `type='unknown'`.
fn insert_synthesized_transactions(
    tx: &Transaction<'_>,
    items: &[LineItem],
    header_map: &HashMap<String, TransactionHeader>,
) -> Result<()> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&LineItem>> = HashMap::new();
    for li in items {
        groups
            .entry(li.invoice_id.clone())
            .or_insert_with(|| {
                order.push(li.invoice_id.clone());
                Vec::new()
            })
            .push(li);
    }

    let mut update_stmt = tx.prepare(
        "UPDATE transactions SET line_count=?, total_cents=?, has_deferred=? WHERE qb_id=?",
    )?;
    let mut insert_stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, \
          line_count, total_cents, has_deferred, source_page) \
         VALUES (?, 'unknown', ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for qb_id in order {
        let lines = &groups[&qb_id];
        let total: i64 = lines
            .iter()
            .filter_map(|l| l.amount_cents.map(|c| c as i64))
            .sum();
        let has_deferred = lines.iter().any(|l| l.amount_type == AmountType::Deferred) as i64;
        let line_count = lines.len() as i64;
        if header_map.contains_key(&qb_id) {
            update_stmt.execute(params![line_count, total, has_deferred, qb_id])?;
            continue;
        }
        let date = lines.iter().find_map(|l| l.txn_date_raw);
        let counter = lines.iter().find_map(|l| l.counter);
        let src = lines
            .iter()
            .find_map(|l| l.source_table.clone())
            .unwrap_or_default();
        let page = lines.first().map(|l| l.page_number as i64).unwrap_or(0);
        insert_stmt.execute(params![
            qb_id,
            src,
            date,
            counter,
            line_count,
            total,
            has_deferred,
            page
        ])?;
    }
    Ok(())
}

fn insert_transaction_line_items(tx: &Transaction<'_>, items: &[LineItem]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO transaction_line_items \
         (qb_id_parent, line_number, item_qb_id, amount_type, amount_cents, \
          amount_cents_signed, amount_raw_hex, txn_date_raw, counter, source_table, \
          page_number, page_offset) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut line_no: BTreeMap<(String, String), i64> = BTreeMap::new();
    for li in items {
        let src = li.source_table.clone().unwrap_or_default();
        let key = (li.invoice_id.clone(), src.clone());
        let n = line_no.entry(key).or_insert(0);
        *n += 1;
        let type_byte = amount_type_byte(li.amount_type);
        let hex = li
            .amount_raw
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        stmt.execute(params![
            li.invoice_id,
            *n,
            li.item_qb_id,
            type_byte,
            li.amount_cents,
            li.amount_cents_signed,
            hex,
            li.txn_date_raw,
            li.counter,
            src,
            li.page_number as i64,
            li.page_offset as i64,
        ])?;
    }
    Ok(())
}

fn amount_type_byte(t: AmountType) -> i64 {
    match t {
        AmountType::None => 0,
        AmountType::OneByteOne => 1,
        AmountType::Standard => 2,
        AmountType::Deferred => 3,
        AmountType::Other(b) => b as i64,
    }
}

#[derive(Debug)]
struct ExportStats {
    pages: u64,
    transactions: i64,
    headers: i64,
    line_items: i64,
    grand_total_cents: i64,
}

impl ExportStats {
    fn summary(&self) -> String {
        format!(
            "openqbw: pages={} transactions={} headers={} lineitems={} grand_total=${:.2}",
            self.pages,
            self.transactions,
            self.headers,
            self.line_items,
            self.grand_total_cents as f64 / 100.0,
        )
    }
}

fn collect_export_stats(
    conn: &Connection,
    store: &PageStore,
    items: &[LineItem],
    headers: &[TransactionHeader],
) -> Result<ExportStats> {
    let txns: i64 = conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(total_cents), 0) FROM transactions",
        [],
        |r| r.get(0),
    )?;
    Ok(ExportStats {
        pages: store.page_count(),
        transactions: txns,
        headers: headers.len() as i64,
        line_items: items.len() as i64,
        grand_total_cents: total,
    })
}

// =========================================================================
// migrate -- data-liberation export
// =========================================================================

fn run_migrate(input: PathBuf, out: PathBuf, format: MigrateFormat) -> Result<()> {
    match format {
        MigrateFormat::Sqlite => run_export(input, out).map(|s| println!("{}", s.summary())),
        MigrateFormat::Csv => run_migrate_csv(input, out),
        MigrateFormat::Iif => run_migrate_iif(input, out),
    }
}

fn collect_records(
    input: &PathBuf,
) -> Result<(Vec<LineItem>, Vec<TransactionHeader>, PageStore, ApModel)> {
    let store = PageStore::open(input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);
    let mut items: Vec<LineItem> =
        iter_lineitems_with_attribution(&store, &model, &attribution).collect();
    items.sort_by_key(|li| (li.page_number, li.page_offset));
    let mut headers: Vec<TransactionHeader> =
        iter_transaction_headers(&store, &model, &attribution).collect();
    headers.sort_by_key(|h| (h.page_number, h.page_offset));
    Ok((items, headers, store, model))
}

/// Minimal CSV-field escaper: quote if the field contains comma, quote,
/// CR, or LF, doubling embedded quotes.
fn csv_field(s: &str) -> String {
    if s.bytes().any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}

fn iso_date_from_unix_days(days: i64) -> String {
    // 1970-01-01 was a Thursday; we want a calendar date. Use chrono-free
    // implementation: 1970-01-01 + days.
    // Simple algorithm via days-from-civil; good for the [1900, 2400] range.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn lineitem_date(li: &LineItem) -> String {
    li.txn_date_days_since_unix()
        .map(iso_date_from_unix_days)
        .unwrap_or_default()
}

fn run_migrate_csv(input: PathBuf, out: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&out).with_context(|| format!("creating {:?}", out))?;
    let (items, headers, store, model) = collect_records(&input)?;

    // catalog.csv
    let mut cat = std::fs::File::create(out.join("catalog.csv"))?;
    use std::io::Write;
    writeln!(cat, "table_id,name,column_count,data_root_page,last_page")?;
    let mut tables: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
    tables.sort_by_key(|e| e.table_id);
    for t in &tables {
        writeln!(
            cat,
            "{},{},{},{},{}",
            t.table_id,
            csv_field(&t.name),
            t.col_count.map(|c| c.to_string()).unwrap_or_default(),
            t.data_root_page.map(|p| p.to_string()).unwrap_or_default(),
            t.last_page.map(|p| p.to_string()).unwrap_or_default(),
        )?;
    }

    // transactions.csv
    let mut txf = std::fs::File::create(out.join("transactions.csv"))?;
    writeln!(txf, "qb_id,txn_type,source_table,page_number,page_offset")?;
    for h in &headers {
        writeln!(
            txf,
            "{},{},{},{},{}",
            csv_field(&h.qb_id),
            csv_field(h.txn_type()),
            csv_field(&h.source_table),
            h.page_number,
            h.page_offset,
        )?;
    }

    // lineitems.csv
    let mut lif = std::fs::File::create(out.join("lineitems.csv"))?;
    writeln!(
        lif,
        "invoice_id,item_qb_id,amount_cents,amount_signed_cents,amount_decimal,txn_date,source_table,page_number,page_offset"
    )?;
    for li in &items {
        let amt = li.amount_cents.map(|c| c.to_string()).unwrap_or_default();
        let amts = li
            .amount_cents_signed
            .map(|c| c.to_string())
            .unwrap_or_default();
        let dec = li
            .amount_cents
            .map(|c| format!("{:.2}", c as f64 / 100.0))
            .unwrap_or_default();
        writeln!(
            lif,
            "{},{},{},{},{},{},{},{},{}",
            csv_field(&li.invoice_id),
            csv_field(li.item_qb_id.as_deref().unwrap_or("")),
            amt,
            amts,
            dec,
            lineitem_date(li),
            csv_field(li.source_table.as_deref().unwrap_or("")),
            li.page_number,
            li.page_offset,
        )?;
    }

    println!(
        "openqbw migrate csv: out={:?} tables={} transactions={} lineitems={}",
        out,
        tables.len(),
        headers.len(),
        items.len(),
    );
    Ok(())
}

fn run_migrate_iif(input: PathBuf, out: PathBuf) -> Result<()> {
    let (items, headers, _store, _model) = collect_records(&input)?;
    use std::io::Write;
    let mut f = std::fs::File::create(&out).with_context(|| format!("creating {:?}", out))?;

    // IIF requires CRLF line endings for maximum compatibility with the
    // Windows tools that consume it.
    let nl = "\r\n";
    write!(
        f,
        "!TRNS\tTRNSID\tTRNSTYPE\tDATE\tACCNT\tNAME\tAMOUNT\tMEMO{nl}"
    )?;
    write!(
        f,
        "!SPL\tSPLID\tTRNSTYPE\tDATE\tACCNT\tNAME\tAMOUNT\tMEMO{nl}"
    )?;
    write!(f, "!ENDTRNS{nl}")?;

    // Bucket line items by parent invoice id. We emit one TRNS per parent
    // group: in this build most parents are orphans (no matching header)
    // because the WP-6Z work only parses the lineitem and a small slice of
    // header tables. We still emit them as TRNS/SPL pairs so the IIF is
    // useful for liberation. When a matching header exists, we use its
    // txn_type; otherwise we default to GENERAL JOURNAL.
    let mut by_parent: HashMap<&str, Vec<&LineItem>> = HashMap::new();
    for li in &items {
        by_parent
            .entry(li.invoice_id.as_str())
            .or_default()
            .push(li);
    }
    let header_by_id: HashMap<&str, &TransactionHeader> =
        headers.iter().map(|h| (h.qb_id.as_str(), h)).collect();
    let mut parent_ids: Vec<&str> = by_parent.keys().copied().collect();
    parent_ids.sort_unstable();

    let mut trns_id: u64 = 1;
    let mut spl_id: u64 = 1;
    let mut written_txns = 0usize;
    let mut written_lines = 0usize;

    for parent_id in parent_ids {
        let kids = by_parent.get(parent_id).cloned().unwrap_or_default();
        if kids.is_empty() {
            continue;
        }
        let sum_cents: i64 = kids
            .iter()
            .map(|li| li.amount_cents.unwrap_or(0) as i64)
            .sum();
        let date = kids
            .iter()
            .find_map(|li| li.txn_date_days_since_unix())
            .map(iso_date_from_unix_days)
            .unwrap_or_default();
        let ttype = header_by_id
            .get(parent_id)
            .map(|h| h.txn_type().to_uppercase())
            .unwrap_or_else(|| "GENERAL JOURNAL".to_string());
        let amt = sum_cents as f64 / 100.0;
        let memo = format_args!("qb_id={parent_id}").to_string();
        write!(
            f,
            "TRNS\t{trns_id}\t{ttype}\t{date}\tAccounts Receivable\t\t{amt:.2}\t{memo}{nl}"
        )?;
        for li in &kids {
            let amt = -(li.amount_cents.unwrap_or(0) as i64) as f64 / 100.0;
            let date = lineitem_date(li);
            let accnt = li.source_table.as_deref().unwrap_or("");
            let memo = li.item_qb_id.as_deref().unwrap_or("");
            write!(
                f,
                "SPL\t{spl_id}\t{ttype}\t{date}\t{accnt}\t\t{amt:.2}\t{memo}{nl}"
            )?;
            spl_id += 1;
            written_lines += 1;
        }
        write!(f, "ENDTRNS{nl}")?;
        trns_id += 1;
        written_txns += 1;
    }

    println!(
        "openqbw migrate iif: out={:?} transactions={} lineitems={}",
        out, written_txns, written_lines
    );
    Ok(())
}

// =========================================================================
// forensics -- file-level discovery
// =========================================================================

fn run_forensics(input: PathBuf) -> Result<()> {
    let store = PageStore::open(&input).with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);

    let file_size = std::fs::metadata(&input)
        .map(|m| m.len())
        .unwrap_or_default();
    let page_count = store.page_count();

    println!("=== file ===");
    println!("path        : {:?}", input);
    println!(
        "size        : {} bytes ({:.1} MiB)",
        file_size,
        file_size as f64 / 1024.0 / 1024.0
    );
    println!("pages       : {} (4096 B each)", page_count);
    println!(
        "ap learned  : {} / {} blocks ({:.1}%)",
        model.learned_block_count(),
        page_count.div_ceil(16),
        100.0 * model.learned_block_count() as f64 / page_count.div_ceil(16).max(1) as f64,
    );

    let tables: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
    let user_tables: Vec<&SysTableEntry> =
        tables.iter().filter(|t| !is_system_name(&t.name)).collect();
    println!();
    println!("=== catalog ===");
    println!("tables total: {}", tables.len());
    println!("tables user : {}", user_tables.len());

    let items: Vec<LineItem> =
        iter_lineitems_with_attribution(&store, &model, &attribution).collect();
    let headers: Vec<TransactionHeader> =
        iter_transaction_headers(&store, &model, &attribution).collect();

    println!();
    println!("=== business records ===");
    println!("transaction headers : {}", headers.len());
    println!("line items          : {}", items.len());

    // Distinct invoice IDs in line items vs in headers; gaps suggest
    // deleted parents.
    use std::collections::HashSet;
    let header_ids: HashSet<&str> = headers.iter().map(|h| h.qb_id.as_str()).collect();
    let parent_ids: HashSet<&str> = items.iter().map(|li| li.invoice_id.as_str()).collect();
    let orphan_parents = parent_ids.difference(&header_ids).count();
    let childless_headers = header_ids.difference(&parent_ids).count();
    let grand_total_cents: i64 = items
        .iter()
        .map(|li| li.amount_cents.unwrap_or(0) as i64)
        .sum();

    println!("distinct parent invoice ids : {}", parent_ids.len());
    println!("orphan parents (no header)  : {}", orphan_parents);
    println!("childless headers           : {}", childless_headers);
    println!(
        "lineitem grand total        : ${:.2}",
        grand_total_cents as f64 / 100.0
    );

    if orphan_parents > 0 || childless_headers > 0 {
        println!();
        println!(
            "note: a non-zero orphan count is a discovery signal -- the file \n      may contain partially-purged records or a parent table this \n      build does not parse yet."
        );
    }

    Ok(())
}
