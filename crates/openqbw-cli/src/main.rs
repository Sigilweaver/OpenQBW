//! `openqbw` command-line tool.
//!
//! Subcommands:
//!
//! ```text
//! openqbw export    <input.qbw> <output.db>   # multi-transaction SQLite export
//! openqbw catalog   <input.qbw>               # SYSTABLE listing
//! openqbw schema    <input.qbw> <table>       # recovered columns of a table
//! openqbw migrate   <input.qbw> --format=...  # data-liberation export (csv/sqlite/iif)
//! openqbw verify    <input.qbw>               # validation summary
//! openqbw debug     <command> <input.qbw>     # reverse-engineering diagnostics
//! ```
//!
//! The `debug` commands print heuristic cross-checks for people studying
//! the format. Their names, flags and output may change in any release.

mod outfile;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use openqbw::diagnostics::{self, CrossValidation};
use openqbw::{
    Decimal, Error as QbwError, LineItem, ReadOptions, Reader, SysTableEntry, TransactionHeader,
    Warning, WarningKind,
};
use outfile::{Staged, StagedSet};
use rusqlite::{Connection, Transaction, params};

/// Version of the SQLite export layout, stored in `PRAGMA user_version` and
/// in `meta.format_version`. Adding a table or column keeps it; renaming or
/// removing one, or changing what a column means, increments it.
const SQLITE_FORMAT_VERSION: i64 = 1;

/// Version of the CSV export layout, stored in `manifest.json`. Same rule as
/// [`SQLITE_FORMAT_VERSION`].
const CSV_FORMAT_VERSION: u32 = 1;

/// What `source_table` is, recorded in every export that has a place for it.
const SOURCE_TABLE_ATTRIBUTION: &str = "position-based, unverified";

/// Exit status of `verify` when a check reports DIFF.
const EXIT_VERIFY_MISMATCH: u8 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "openqbw",
    version,
    about = "QuickBooks .qbw inspector and exporter"
)]
struct Cli {
    /// Skip records and pages that cannot be decoded (value length byte
    /// above 10, value field cut off by the page end, a page that fails to
    /// decode)
    /// instead of stopping with an error. Every skipped record is printed on
    /// stderr and listed in the export (`scan_rejections` table in SQLite,
    /// `rejected.csv` in CSV).
    #[arg(long, global = true)]
    lenient: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Export transactions and line items to SQLite, attributing each
    /// line item to its source table via SYSTABLE. The attribution is by
    /// page position only and is unverified (`meta.source_table_attribution`).
    /// Each line item carries its numeric value field as an exact decimal;
    /// that the value is the line's amount is not established.
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
    /// Export, then report per-table coverage, decoded and undecoded values,
    /// and how many parent groups of three or more lines sum to exactly zero.
    /// The value is not established as the line amount and source_table is an
    /// unverified, position-based attribution.
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
    /// bridged to SYSTABLE.name via the table-id back-reference in the
    /// SYSTABLE row prefix, falling back to the SYSOBJECT catalog scan
    /// (Phase 6, WP-6Z.2 / WP-6Z.4).
    Schema {
        /// Input QBW file.
        input: PathBuf,
        /// Table name to inspect.
        table: String,
    },
    /// Data-liberation export: emit line items and transaction
    /// headers in a portable format suitable for handing to a
    /// different accounting product or for archival. `source_table` is
    /// position-based and unverified; `value` is the line's numeric value
    /// field, not established as its amount.
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
        /// IIF only: write the export even though account names and
        /// transaction types are not decoded. Every TRNS posts to the
        /// placeholder account `Accounts Receivable` and every SPL uses the
        /// internal source-table name as its account. Transactions with an
        /// undecoded value are left out and counted.
        #[arg(long)]
        placeholder_accounts: bool,
    },
    /// Reverse-engineering diagnostics. Names, flags and output of these
    /// commands may change in any release.
    Debug {
        #[command(subcommand)]
        cmd: DebugCmd,
    },
}

#[derive(Subcommand, Debug)]
enum DebugCmd {
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

/// Open a company file. Opening checks the magic, version and every page
/// CRC and reads the catalog; its warnings and skipped pages are printed.
fn open_reader(path: &std::path::Path, lenient: bool) -> Result<Reader> {
    let options = if lenient {
        ReadOptions::lenient()
    } else {
        ReadOptions::strict()
    };
    let reader = Reader::open(path, &options).with_context(|| {
        format!(
            "opening {:?}{}",
            path,
            if lenient {
                ""
            } else {
                " (use --lenient to skip pages that fail to decode and list them)"
            }
        )
    })?;
    report(&reader.catalog().warnings, &reader.catalog().rejected);
    Ok(reader)
}

/// Print run-level warnings and skipped records on stderr.
fn report(warnings: &[Warning], rejected: &[QbwError]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
    for e in rejected {
        eprintln!("warning: skipped: {e}");
    }
}

/// Exit status: 0 success, 1 error, 2 usage error (from the argument
/// parser), 3 `verify` found a check that differs.
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let lenient = cli.lenient;
    if let Cmd::Verify {
        input,
        output,
        strict_attribution,
    } = cli.cmd
    {
        let ok = run_verify(input, output, strict_attribution, lenient)?;
        return Ok(if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(EXIT_VERIFY_MISMATCH)
        });
    }
    match cli.cmd {
        Cmd::Export { input, output } => run_export(input, output, lenient).map(|_| ()),
        Cmd::Catalog { input, user_only } => run_catalog(input, user_only, lenient),
        Cmd::Verify { .. } => unreachable!("handled above"),
        Cmd::Schema { input, table } => run_schema(input, table, lenient),
        Cmd::Migrate {
            input,
            out,
            format,
            placeholder_accounts,
        } => run_migrate(input, out, format, placeholder_accounts, lenient),
        Cmd::Debug { cmd } => match cmd {
            DebugCmd::Fkgraph {
                input,
                resolved_only,
            } => run_fkgraph(input, resolved_only, lenient),
            DebugCmd::Nulls { input } => run_nulls(input, lenient),
            DebugCmd::ValidateAttribution { input } => run_validate_attribution(input, lenient),
            DebugCmd::Indexes {
                input,
                fk_only,
                summary_only,
            } => run_indexes(input, fk_only, summary_only, lenient),
            DebugCmd::Forensics { input } => run_forensics(input, lenient),
        },
    }?;
    Ok(ExitCode::SUCCESS)
}

/// Export to SQLite. Nothing at `output` is touched until the input has
/// opened and scanned and the new database is complete: it is built in a
/// temporary file next to `output` and renamed over it.
fn run_export(input: PathBuf, output: PathBuf, lenient: bool) -> Result<ExportStats> {
    outfile::check_output(&input, &output)?;
    let reader = open_reader(&input, lenient)?;
    let Records {
        items,
        rejected,
        headers,
        counts,
        ..
    } = collect_records(&reader)?;

    let staged = Staged::new(&output);
    // A leftover from a crashed run with the same process id.
    let _ = std::fs::remove_file(staged.path());
    let mut conn =
        Connection::open(staged.path()).with_context(|| format!("creating {:?}", output))?;
    create_schema(&conn)?;

    {
        let tx = conn.transaction()?;
        let header_map = insert_transaction_headers(&tx, &headers)?;
        insert_synthesized_transactions(&tx, &items, &header_map)?;
        insert_transaction_line_items(&tx, &items)?;
        insert_scan_rejections(&tx, &rejected)?;
        insert_counts(&tx, &counts)?;
        tx.commit()?;
    }

    let stats = collect_export_stats(&conn, &reader, &items, &headers, rejected.len())?;
    conn.close()
        .map_err(|(_, e)| e)
        .with_context(|| format!("closing {:?}", output))?;
    staged.commit()?;
    println!("{}", stats.summary());
    Ok(stats)
}

fn run_catalog(input: PathBuf, user_only: bool, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;

    let mut entries: Vec<SysTableEntry> = reader.catalog().items.clone();
    entries.sort_by_key(|e| e.table_id);

    let total = entries.len();
    let user: Vec<&SysTableEntry> = entries
        .iter()
        .filter(|e| !is_system_name(&e.name))
        .collect();

    println!(
        "file: {}  pages={}  unique_tables={}  user_tables={}",
        input.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        reader.page_count(),
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

fn run_schema(input: PathBuf, table: String, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;
    let found = reader.schema(&table)?;
    report(&found.warnings, &[]);
    if found.pages_rejected > 0 {
        eprintln!(
            "warning: {} pages failed to decode and were skipped",
            found.pages_rejected
        );
    }
    let cols = &found.columns;
    if cols.is_empty() {
        anyhow::bail!("{}", schema_miss_report(&found));
    }
    let provenance = match (found.owner, found.source) {
        (Some(owner), Some(source)) => {
            format!("  (SYSCOLUMN owner {}, via {})", owner, source.label())
        }
        _ => String::new(),
    };
    println!("table: {}  columns: {}{}", table, cols.len(), provenance);
    if found.has_column_gaps()
        && let Some((first, last)) = found.column_id_range()
    {
        eprintln!(
            "warning: {} of the {} column_ids in {}..={} have no recovered SYSCOLUMN row, \
             so the listing below is incomplete (openqbw#19)",
            found.missing_column_ids(),
            (last - first) as usize + 1,
            first,
            last
        );
    }
    if let Some(short) = found.columns_short_of_catalog()
        && short > 0
    {
        eprintln!(
            "warning: SYSTABLE declares {} columns for {}, {} more than were recovered, so the \
             listing below is incomplete",
            found.catalog_col_count.unwrap_or_default(),
            table,
            short
        );
    }
    if found.candidate_owners.len() > 1 {
        eprintln!(
            "warning: {} SYSCOLUMN owners resolved to {}: {:?}; the one with the most columns \
             was listed",
            found.candidate_owners.len(),
            table,
            found.candidate_owners
        );
    }
    let st = &found.scan_stats;
    let unread = st.rows_no_name + st.rows_ambiguous_name + st.rows_bad_layout;
    if unread > 0 || st.pages_without_directory > 0 || st.pages_unconfirmed_directory > 0 {
        eprintln!(
            "warning: SYSCOLUMN scan: {} of {} rows not read ({} without a name, {} with an \
             ambiguous name, {} with an unexpected layout); {} pages skipped for lack of a \
             slot directory, {} because their slot directory is not confirmed",
            unread,
            st.tags_seen,
            st.rows_no_name,
            st.rows_ambiguous_name,
            st.rows_bad_layout,
            st.pages_without_directory,
            st.pages_unconfirmed_directory
        );
    }
    if found.implausible_column_ids() > 0 {
        eprintln!(
            "warning: {} row(s) below carry an implausible column_id and are likely \
             mis-parsed",
            found.implausible_column_ids()
        );
    }
    println!(
        "{:>5}  {:<32}  {:>6}  {:>5}  {:>5}",
        "id", "name", "domain", "width", "nulls"
    );
    for c in cols {
        println!(
            "{:>5}  {:<32}  {:>6}  {:>5}  {:>5}",
            c.column_id, c.name, c.domain_char as char, c.width, c.nulls_flag
        );
    }
    Ok(())
}

/// Explain a `schema` miss in terms of the stage that actually failed:
/// the catalog lookup, `SYSCOLUMN` recovery, or the owner -> table
/// bridge. A table can be listed by `catalog` and still have no
/// recoverable schema, and saying which is the case is the whole point
/// (openqbw#19).
fn schema_miss_report(found: &openqbw::SchemaRecovery) -> String {
    let mut out = format!("no columns recovered for table {:?}\n", found.table);
    out.push_str(&match found.catalog_table_id {
        Some(tid) => format!("  catalog:      listed in SYSTABLE (table_id {})\n", tid),
        None => {
            "  catalog:      no SYSTABLE row with this name - check `openqbw catalog`\n".to_string()
        }
    });
    out.push_str(&format!(
        "  SYSCOLUMN:    {} rows recovered, {} distinct owners\n",
        found.syscolumn_rows, found.distinct_owners
    ));
    out.push_str(&match found.backref_distance {
        Some(d) => format!(
            "  owner bridge: {} tables ({} via SYSTABLE back-reference at -{}, \
             {} via SYSOBJECT scan)\n",
            found.bridged_tables, found.from_backref, d, found.from_sysobject
        ),
        None => format!(
            "  owner bridge: {} tables ({} via SYSOBJECT scan; no SYSTABLE \
             back-reference distance could be calibrated on this file)\n",
            found.bridged_tables, found.from_sysobject
        ),
    });
    out.push_str(
        "  this table:   no SYSCOLUMN owner could be bridged to it, so its columns \
         cannot be attributed yet",
    );
    out
}

fn run_fkgraph(input: PathBuf, resolved_only: bool, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;
    let graph = diagnostics::fk_graph(&reader)?;
    let s = diagnostics::fk_graph_stats(&graph);
    if graph.pages_rejected > 0 {
        eprintln!(
            "warning: {} pages failed to decode and were skipped",
            graph.pages_rejected
        );
    }
    let edges = &graph.edges;
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
    if s.columns_unbridged > 0 {
        eprintln!(
            "warning: {} of {} *_id columns belong to a table that could not be named and are not in the graph",
            s.columns_unbridged, graph.columns_considered
        );
    }
    println!(
        "{:<40}  {:>4}  {:<28}  {:<40}  {:>5}",
        "source_table", "col", "source_column", "target_table", "score"
    );
    for e in edges {
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

fn run_indexes(input: PathBuf, fk_only: bool, summary_only: bool, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;
    let scan = reader.indexes()?;
    report(&scan.warnings, &scan.rejected);
    let entries = &scan.items;
    let tables = &reader.catalog().items;
    let audit = diagnostics::sysindex_cross_validation(&reader)?;

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
    for t in tables {
        name_by_tid
            .entry(t.table_id)
            .or_insert_with(|| t.name.clone());
    }
    println!();
    println!("{:>8}  {:>8}  {:<40}  index_name", "tid", "root", "owner");
    for e in entries {
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

fn run_validate_attribution(input: PathBuf, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;
    let position = reader.attribution();
    let schema = diagnostics::schema_attribution(&reader)?;
    println!(
        "tables with width bands: {}  total pages: {}",
        schema.len(),
        reader.page_count()
    );
    let pairs = (1..reader.page_count())
        .filter_map(|pn| position.attribute(pn).map(|e| (pn, e.name.clone())));
    let stats = schema.validate_corpus(&reader, pairs);
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

fn run_nulls(input: PathBuf, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;
    let scan = diagnostics::nulls_flag_histogram(&reader)?;
    report(&scan.warnings, &scan.rejected);
    let buckets = &scan.items;
    let total: usize = buckets.iter().map(|b| b.count).sum();
    println!(
        "total syscolumn rows: {}  distinct nulls_flag values: {}",
        total,
        buckets.len()
    );
    println!("{:>6}  {:>8}  sample_columns", "byte", "count");
    for b in buckets {
        let samples = b.sample_columns.join(", ");
        println!("0x{:02x}    {:>8}  {}", b.flag, b.count, samples);
    }
    Ok(())
}

/// Run the checks; `Ok(false)` when any reports DIFF.
fn run_verify(
    input: PathBuf,
    output: Option<PathBuf>,
    strict_attribution: bool,
    lenient: bool,
) -> Result<bool> {
    let out = match output {
        Some(p) => p,
        None => std::env::temp_dir().join(format!("openqbw-verify-{}.sqlite", std::process::id())),
    };
    let stats = run_export(input.clone(), out.clone(), lenient)?;
    println!();
    println!("=== verification report ===");

    let conn = Connection::open(&out)?;
    check_export_format(&conn).with_context(|| format!("reading {:?}", out))?;
    let diffs: Vec<&str> = Vec::new();
    println!();
    println!("Per source_table line-item counts (source_table is position-based and unverified):");
    println!(
        "  {:44}  {:>8}  {:>8}  sum of decoded values",
        "source_table", "lines", "decoded"
    );
    for (t, row) in &stats.per_table {
        println!(
            "  {:44}  {:>8}  {:>8}  {}",
            t.as_deref().unwrap_or(UNATTRIBUTED),
            row.lines,
            row.decoded,
            row.sum.map_or("overflow".to_owned(), |d| d.to_string()),
        );
    }

    println!();
    println!("Per type transaction counts:");
    let mut stmt = conn.prepare(
        "SELECT type, COUNT(*), COALESCE(SUM(line_count), 0), COALESCE(SUM(undecoded_lines), 0)
         FROM transactions
         GROUP BY type
         ORDER BY type",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    println!(
        "  {:44}  {:>8}  {:>8}  {:>10}",
        "type", "txns", "lines", "undecoded"
    );
    for row in rows {
        let (t, n, lines, undecoded) = row?;
        println!("  {:44}  {:>8}  {:>8}  {:>10}", t, n, lines, undecoded);
    }

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
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (t, n) = row?;
        println!("  {:44}  {:>8}", t.as_deref().unwrap_or(UNATTRIBUTED), n);
    }

    println!();
    println!("Parent-group balance (informational; not a pass/fail check):");
    println!(
        "  {} undecoded values: {} zero-length, {} digit above 99, {} exponent out of range",
        stats.undecoded.total(),
        stats.undecoded.zero_length,
        stats.undecoded.digit_over_99,
        stats.undecoded.exponent_out_of_range,
    );
    println!(
        "  parent groups of 3+ lines, all decoded: {}; summing to exactly zero: {} ({:.1}%)",
        stats.balance.groups,
        stats.balance.balanced,
        100.0 * stats.balance.balanced as f64 / stats.balance.groups.max(1) as f64,
    );
    println!("  groups are by page-position parent QB-ID; the value is not established as the");
    println!("  line amount, so a zero sum is evidence about the encoding, not an invoice check.");

    println!();
    println!("=== SYSINDEX cross-validation (WP-6Z.3) ===");
    let reader = open_reader(&input, lenient)?;
    {
        let entries = reader.indexes()?.items;
        let audit = diagnostics::sysindex_cross_validation(&reader)?;
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
        let content = diagnostics::content_attribution(&reader)?;
        let position = reader.attribution();
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
            .map(|r| r.map(|n| n as u64))
            .collect::<rusqlite::Result<_>>()?;
        let agree = content.compare(&reader, position, pages.iter().copied());
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
    if diffs.is_empty() {
        Ok(true)
    } else {
        eprintln!("verify: checks that differ: {}", diffs.join(", "));
        Ok(false)
    }
}

/// Label for rows without a source table in human-readable output.
const UNATTRIBUTED: &str = "(unattributed)";

/// Refuse an SQLite file this build cannot read: one that is not an
/// openqbw export, or one written in a later format version. `verify` runs it
/// on the file it has just written, as a check on the writer.
fn check_export_format(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let meta: Option<String> = match conn.query_row(
        "SELECT value FROM meta WHERE key = 'format_version'",
        [],
        |r| r.get(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        // A file without a meta table is not an openqbw export.
        Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.starts_with("no such table") => None,
        Err(e) => return Err(e).context("reading meta.format_version"),
    };
    let Some(meta) = meta else {
        anyhow::bail!("not an openqbw export: no meta.format_version");
    };
    let meta: i64 = meta
        .parse()
        .with_context(|| format!("meta.format_version {meta:?} is not an integer"))?;
    if meta != user_version {
        anyhow::bail!(
            "meta.format_version {meta} disagrees with PRAGMA user_version {user_version}"
        );
    }
    if meta > SQLITE_FORMAT_VERSION {
        anyhow::bail!(
            "export format version {meta} is newer than this openqbw reads \
             ({SQLITE_FORMAT_VERSION}); use a newer openqbw"
        );
    }
    if meta < 1 {
        anyhow::bail!("not an openqbw export: format version {meta}");
    }
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
            -- NULL when no line's page is attributed to a table.
            source_table TEXT,
            txn_date_raw INTEGER,
            counter      INTEGER,
            date_source  TEXT NOT NULL,
            line_count   INTEGER NOT NULL,
            -- Exact decimal sum of the decoded line values; lines with an
            -- undecoded value are not in it (see undecoded_lines). Not an
            -- invoice total: the value is not established as the line amount.
            value_sum    TEXT,
            undecoded_lines INTEGER NOT NULL,
            source_page  INTEGER
        );

        CREATE TABLE transaction_line_items (
            id                  INTEGER PRIMARY KEY,
            qb_id_parent        TEXT NOT NULL,
            line_number         INTEGER NOT NULL,
            item_qb_id          TEXT,
            -- The numeric value field as an exact decimal string; NULL when
            -- not decoded (see value_undecoded). Not established as the
            -- line's amount.
            value               TEXT,
            -- zero_length, digit_over_99 or exponent_out_of_range; NULL when
            -- the value is decoded.
            value_undecoded     TEXT,
            -- The field's bytes, lowercase hex: [k][ctrl][k digit bytes].
            value_raw_hex       TEXT NOT NULL,
            txn_date_raw        INTEGER,
            counter             INTEGER,
            date_source         TEXT NOT NULL,
            -- Comma-separated record flags; empty when none.
            flags               TEXT NOT NULL,
            -- Position-based, unverified attribution (see
            -- meta.source_table_attribution). NULL when the line's page is
            -- not attributed to a table.
            source_table        TEXT,
            page_number         INTEGER NOT NULL,
            page_offset         INTEGER NOT NULL,
            UNIQUE (page_number, page_offset)
        );

        CREATE TABLE scan_rejections (
            page_number INTEGER,
            page_offset INTEGER,
            kind        TEXT NOT NULL,
            message     TEXT NOT NULL
        );

        CREATE TABLE meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE INDEX idx_lineitems_page   ON transaction_line_items(page_number);
        CREATE INDEX idx_lineitems_source ON transaction_line_items(source_table);
        CREATE INDEX idx_tx_type          ON transactions(type);
        CREATE INDEX idx_tx_source        ON transactions(source_table);
        "#,
    )?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES \
         ('format_version', ?), ('openqbw_version', ?), ('source_table_attribution', ?)",
        params![
            SQLITE_FORMAT_VERSION.to_string(),
            env!("CARGO_PKG_VERSION"),
            SOURCE_TABLE_ATTRIBUTION
        ],
    )?;
    conn.pragma_update(None, "user_version", SQLITE_FORMAT_VERSION)?;
    Ok(())
}

/// Headers seen more than once under the same `qb_id`.
#[derive(Debug, Default, PartialEq, Eq)]
struct HeaderDupes {
    /// Sightings after the first.
    duplicates: usize,
    /// IDs seen under more than one transaction type.
    conflicting_types: usize,
}

/// One header per `qb_id`, keeping the first sighting in the order given
/// (page order), and the count of the others. Every export format applies
/// this rule through [`collect_records`].
fn dedupe_headers(headers: Vec<TransactionHeader>) -> (Vec<TransactionHeader>, HeaderDupes) {
    let mut first_type: HashMap<String, String> = HashMap::new();
    let mut conflicting: std::collections::HashSet<String> = Default::default();
    let mut dupes = HeaderDupes::default();
    let mut unique = Vec::with_capacity(headers.len());
    for h in headers {
        match first_type.get(&h.qb_id) {
            None => {
                first_type.insert(h.qb_id.clone(), h.txn_type().to_owned());
                unique.push(h);
            }
            Some(first) => {
                dupes.duplicates += 1;
                if first != h.txn_type() {
                    conflicting.insert(h.qb_id);
                }
            }
        }
    }
    dupes.conflicting_types = conflicting.len();
    (unique, dupes)
}

/// Insert one row per header. `headers` holds one header per `qb_id`
/// (see [`dedupe_headers`]).
/// Record the run counts in `meta` as `count.<scan>.<name>`.
fn insert_counts(tx: &Transaction<'_>, counts: &RunCounts) -> Result<()> {
    let mut stmt = tx.prepare("INSERT INTO meta (key, value) VALUES (?, ?)")?;
    for (key, value) in counts.iter() {
        stmt.execute(params![format!("count.{key}"), value.to_string()])?;
    }
    Ok(())
}

fn insert_transaction_headers(
    tx: &Transaction<'_>,
    headers: &[TransactionHeader],
) -> Result<HashMap<String, TransactionHeader>> {
    let map: HashMap<String, TransactionHeader> = headers
        .iter()
        .map(|h| (h.qb_id.clone(), h.clone()))
        .collect();
    let mut stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, date_source, \
          line_count, value_sum, undecoded_lines, source_page) \
         VALUES (?, ?, ?, ?, ?, ?, 0, NULL, 0, ?)",
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
            h.date_source.as_str(),
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
        "UPDATE transactions SET line_count=?, value_sum=?, undecoded_lines=? \
         WHERE qb_id=?",
    )?;
    let mut insert_stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, date_source, \
          line_count, value_sum, undecoded_lines, source_page) \
         VALUES (?, 'unknown', ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for qb_id in order {
        let lines = &groups[&qb_id];
        let undecoded = lines.iter().filter(|l| l.value.is_none()).count() as i64;
        // Exact sum of the decoded values; NULL only if it overflows.
        let value_sum: Option<String> =
            Decimal::checked_sum(lines.iter().filter_map(|l| l.value)).map(|d| d.to_string());
        let line_count = lines.len() as i64;
        if header_map.contains_key(&qb_id) {
            update_stmt.execute(params![line_count, value_sum, undecoded, qb_id])?;
            continue;
        }
        let date = lines.iter().find_map(|l| l.txn_date_raw);
        let counter = lines.iter().find_map(|l| l.counter);
        let date_source = lines
            .iter()
            .find(|l| l.txn_date_raw.is_some())
            .or(lines.first())
            .map(|l| l.date_source)
            .unwrap_or(openqbw::DateSource::NotFound);
        let src: Option<String> = lines.iter().find_map(|l| l.source_table.clone());
        let page = lines.first().map(|l| l.page_number as i64).unwrap_or(0);
        insert_stmt.execute(params![
            qb_id,
            src,
            date,
            counter,
            date_source.as_str(),
            line_count,
            value_sum,
            undecoded,
            page
        ])?;
    }
    Ok(())
}

fn insert_transaction_line_items(tx: &Transaction<'_>, items: &[LineItem]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO transaction_line_items \
         (qb_id_parent, line_number, item_qb_id, value, value_undecoded, \
          value_raw_hex, txn_date_raw, counter, date_source, \
          flags, source_table, page_number, page_offset) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut line_no: BTreeMap<(String, Option<String>), i64> = BTreeMap::new();
    for li in items {
        let src = li.source_table.clone();
        let key = (li.invoice_id.clone(), src.clone());
        let n = line_no.entry(key).or_insert(0);
        *n += 1;
        stmt.execute(params![
            li.invoice_id,
            *n,
            li.item_qb_id,
            li.value.map(|v| v.to_string()),
            li.value_undecoded.map(|r| r.as_str()),
            hex(&li.value_raw),
            li.txn_date_raw,
            li.counter,
            li.date_source.as_str(),
            flags_csv(li),
            src,
            li.page_number as i64,
            li.page_offset as i64,
        ])?;
    }
    Ok(())
}

fn flags_csv(li: &LineItem) -> String {
    li.flags
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn insert_scan_rejections(tx: &Transaction<'_>, rejected: &[QbwError]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO scan_rejections (page_number, page_offset, kind, message) VALUES (?, ?, ?, ?)",
    )?;
    for e in rejected {
        stmt.execute(params![
            e.page_number().map(|p| p as i64),
            e.page_offset().map(|o| o as i64),
            e.kind().as_str(),
            e.to_string()
        ])?;
    }
    Ok(())
}

/// Line items whose value was not decoded, by reason.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct UndecodedCounts {
    zero_length: u64,
    digit_over_99: u64,
    exponent_out_of_range: u64,
}

impl UndecodedCounts {
    fn of(items: &[LineItem]) -> Self {
        let mut c = Self::default();
        for reason in items.iter().filter_map(|li| li.value_undecoded) {
            match reason {
                openqbw::UndecodedReason::ZeroLength => c.zero_length += 1,
                openqbw::UndecodedReason::DigitOver99 => c.digit_over_99 += 1,
                openqbw::UndecodedReason::ExponentOutOfRange => c.exponent_out_of_range += 1,
                _ => {}
            }
        }
        c
    }

    fn total(&self) -> u64 {
        self.zero_length + self.digit_over_99 + self.exponent_out_of_range
    }
}

/// Line items and decoded values attributed to one source table.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TableRow {
    lines: u64,
    decoded: u64,
    /// Exact sum of the decoded values; `None` if it overflowed.
    sum: Option<Decimal>,
}

fn per_table(items: &[LineItem]) -> BTreeMap<Option<String>, TableRow> {
    let mut out: BTreeMap<Option<String>, TableRow> = BTreeMap::new();
    for li in items {
        let row = out.entry(li.source_table.clone()).or_insert(TableRow {
            lines: 0,
            decoded: 0,
            sum: Some(Decimal::ZERO),
        });
        row.lines += 1;
        if let Some(v) = li.value {
            row.decoded += 1;
            row.sum = row.sum.and_then(|s| s.checked_add(v));
        }
    }
    out
}

/// Parent groups of three or more lines whose values are all decoded, and
/// how many of them sum to exactly zero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Balance {
    groups: u64,
    balanced: u64,
}

fn balance(items: &[LineItem]) -> Balance {
    let mut by_parent: HashMap<&str, Vec<Option<Decimal>>> = HashMap::new();
    for li in items {
        by_parent
            .entry(li.invoice_id.as_str())
            .or_default()
            .push(li.value);
    }
    let mut b = Balance::default();
    for values in by_parent.values().filter(|v| v.len() >= 3) {
        let Some(values) = values.iter().copied().collect::<Option<Vec<Decimal>>>() else {
            continue;
        };
        b.groups += 1;
        if Decimal::checked_sum(values).is_some_and(|s| s.is_zero()) {
            b.balanced += 1;
        }
    }
    b
}

#[derive(Debug)]
struct ExportStats {
    pages: u64,
    transactions: i64,
    headers: i64,
    line_items: i64,
    /// Exact sum of every decoded line value; `None` if it overflowed. A sum
    /// of values, not of invoice totals.
    value_sum: Option<Decimal>,
    /// Line items whose value is not decoded, by reason.
    undecoded: UndecodedCounts,
    per_table: BTreeMap<Option<String>, TableRow>,
    balance: Balance,
    rejected_records: usize,
}

impl ExportStats {
    fn summary(&self) -> String {
        format!(
            "openqbw: pages={} transactions={} headers={} lineitems={} value_sum={} \
             (sum of decoded line values, not invoice totals; {} line items have undecoded \
             values and are not in it, {} records skipped)",
            self.pages,
            self.transactions,
            self.headers,
            self.line_items,
            self.value_sum
                .map_or("overflow".to_owned(), |d| d.to_string()),
            self.undecoded.total(),
            self.rejected_records,
        )
    }
}

fn collect_export_stats(
    conn: &Connection,
    reader: &Reader,
    items: &[LineItem],
    headers: &[TransactionHeader],
    rejected_records: usize,
) -> Result<ExportStats> {
    let txns: i64 = conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
    Ok(ExportStats {
        value_sum: Decimal::checked_sum(items.iter().filter_map(|li| li.value)),
        undecoded: UndecodedCounts::of(items),
        per_table: per_table(items),
        balance: balance(items),
        rejected_records,
        pages: reader.page_count(),
        transactions: txns,
        headers: headers.len() as i64,
        line_items: items.len() as i64,
    })
}

// =========================================================================
// migrate -- data-liberation export
// =========================================================================

fn run_migrate(
    input: PathBuf,
    out: PathBuf,
    format: MigrateFormat,
    placeholder_accounts: bool,
    lenient: bool,
) -> Result<()> {
    if placeholder_accounts && !matches!(format, MigrateFormat::Iif) {
        anyhow::bail!("--placeholder-accounts applies only to --format iif");
    }
    match format {
        MigrateFormat::Sqlite => {
            run_export(input, out, lenient).map(|s| println!("{}", s.summary()))
        }
        MigrateFormat::Csv => run_migrate_csv(input, out, lenient),
        MigrateFormat::Iif => run_migrate_iif(input, out, placeholder_accounts, lenient),
    }
}

struct Records {
    items: Vec<LineItem>,
    /// Records and pages the line-item and header scans skipped.
    rejected: Vec<QbwError>,
    /// One header per `qb_id`, in page order.
    headers: Vec<TransactionHeader>,
    /// Headers left out because their `qb_id` was seen earlier.
    header_dupes: HeaderDupes,
    /// Warning and duplicate counts to record in the export.
    counts: RunCounts,
}

/// Counts recorded in every export that has somewhere to put them, under
/// the keys `<scan>.<name>` (`catalog`, `line_items` or `headers`). A scan's
/// usual counts are present even when zero, so a missing key never means
/// "zero"; a warning outside the usual list is added when it occurs. The
/// totals and breakdowns a count has (`..._decoded`, `..._extent`, `..._total`,
/// `..._zero_length`) appear
/// only with a nonzero count.
#[derive(Debug, Default)]
struct RunCounts(BTreeMap<String, u64>);

/// Counts every scan reports.
const PAGE_CONFIDENCE_COUNTS: [&str; 2] =
    ["keystream_unverified_pages", "low_confidence_sector_pages"];

impl RunCounts {
    /// Add the counts of one scan's `warnings`, after its usual `keys`.
    fn add_scan(&mut self, scan: &str, keys: &[&str], warnings: &[Warning]) {
        for key in PAGE_CONFIDENCE_COUNTS.iter().chain(keys) {
            self.0.entry(format!("{scan}.{key}")).or_insert(0);
        }
        for w in warnings {
            for (name, n) in warning_values(&w.kind) {
                *self.0.entry(format!("{scan}.{name}")).or_insert(0) += n;
            }
        }
    }

    fn set(&mut self, key: &str, value: u64) {
        self.0.insert(key.to_owned(), value);
    }

    fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// The numbers a warning carries, named by the warning's code. The first is
/// the count of affected items; a second, `<code>_<denominator>`, is the
/// total it is out of.
fn warning_values(kind: &WarningKind) -> Vec<(String, u64)> {
    let code = kind.as_str();
    match kind {
        WarningKind::OpaquePages { opaque, extent, .. } => {
            vec![(code.into(), *opaque), (format!("{code}_extent"), *extent)]
        }
        WarningKind::KeystreamUnverifiedPages { pages, decoded, .. }
        | WarningKind::LowConfidenceSectorPages { pages, decoded, .. } => {
            vec![(code.into(), *pages), (format!("{code}_decoded"), *decoded)]
        }
        WarningKind::UnconfirmedDirectoryPages { pages, .. } => vec![(code.into(), *pages)],
        WarningKind::RecordsOutsideLiveRows { records, .. } => vec![(code.into(), *records)],
        WarningKind::HeaderRowsWithoutId { rows, .. } => vec![(code.into(), *rows)],
        WarningKind::UnattributedRecords { records, total, .. } => {
            vec![(code.into(), *records), (format!("{code}_total"), *total)]
        }
        WarningKind::UndecodedValues {
            zero_length,
            digit_over_99,
            exponent_out_of_range,
            ..
        } => vec![
            (
                code.into(),
                zero_length + digit_over_99 + exponent_out_of_range,
            ),
            (format!("{code}_zero_length"), *zero_length),
            (format!("{code}_digit_over_99"), *digit_over_99),
            (
                format!("{code}_exponent_out_of_range"),
                *exponent_out_of_range,
            ),
        ],
        WarningKind::LineItemsNotRead { anchors, .. } => vec![(code.into(), *anchors)],
        WarningKind::InvalidRootPages { tables, .. } => vec![(code.into(), *tables)],
        // A condition without a count: 1 when present.
        _ => vec![(code.into(), 1)],
    }
}

/// Scan line items and headers. Strict mode stops at the first undecodable
/// record; lenient mode returns the skipped ones and prints each on stderr.
fn collect_records(reader: &Reader) -> Result<Records> {
    let lines = reader
        .line_items()
        .context("scanning line items (use --lenient to skip undecodable records and list them)")?;
    let heads = reader.transactions().context(
        "scanning transaction headers (use --lenient to skip failing pages and list them)",
    )?;
    let mut counts = RunCounts::default();
    counts.add_scan(
        "catalog",
        &["invalid_root_pages"],
        &reader.catalog().warnings,
    );
    counts.add_scan(
        "line_items",
        &[
            "opaque_pages",
            "records_outside_live_rows",
            "line_items_not_read",
            "undecoded_values",
            "unattributed_records",
            "attribution_gap",
        ],
        &lines.warnings,
    );
    counts.add_scan(
        "headers",
        &[
            "unconfirmed_directory_pages",
            "header_rows_without_id",
            "attribution_gap",
        ],
        &heads.warnings,
    );
    let mut warnings = lines.warnings;
    for w in heads.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let mut rejected = lines.rejected;
    rejected.extend(heads.rejected);
    report(&warnings, &rejected);
    let mut items = lines.items;
    items.sort_by_key(|li| (li.page_number, li.page_offset));
    let mut headers = heads.items;
    headers.sort_by_key(|h| (h.page_number, h.page_offset));
    let (headers, header_dupes) = dedupe_headers(headers);
    if header_dupes.duplicates > 0 {
        eprintln!(
            "warning: {} transaction headers repeat an earlier qb_id and were not exported \
             ({} qb_ids appear under more than one transaction type; the first in page order \
             was kept)",
            header_dupes.duplicates, header_dupes.conflicting_types
        );
    }
    counts.set(
        "headers.duplicate_header_ids",
        header_dupes.duplicates as u64,
    );
    counts.set(
        "headers.duplicate_header_ids_conflicting_types",
        header_dupes.conflicting_types as u64,
    );
    Ok(Records {
        items,
        rejected,
        headers,
        header_dupes,
        counts,
    })
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

/// Files `migrate --format csv` writes into its output directory.
const CSV_FILES: [&str; 5] = [
    "catalog.csv",
    "transactions.csv",
    "lineitems.csv",
    "rejected.csv",
    "manifest.json",
];

fn run_migrate_csv(input: PathBuf, out: PathBuf, lenient: bool) -> Result<()> {
    if out.exists() && !out.is_dir() {
        anyhow::bail!(
            "{:?} exists and is not a directory; nothing was changed",
            out
        );
    }
    outfile::check_output(&input, &out)?;
    for name in CSV_FILES {
        outfile::check_output(&input, &out.join(name))?;
    }
    let reader = open_reader(&input, lenient)?;
    let Records {
        items,
        rejected,
        headers,
        header_dupes,
        counts,
    } = collect_records(&reader)?;

    // catalog.csv
    use std::io::Write;
    let mut cat: Vec<u8> = Vec::new();
    writeln!(cat, "table_id,name,column_count,data_root_page,last_page")?;
    let mut tables: Vec<SysTableEntry> = reader.catalog().items.clone();
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
    let mut txf: Vec<u8> = Vec::new();
    writeln!(
        txf,
        "qb_id,txn_type,source_table,txn_date_raw,counter,date_source,page_number,page_offset"
    )?;
    for h in &headers {
        writeln!(
            txf,
            "{},{},{},{},{},{},{},{}",
            csv_field(&h.qb_id),
            csv_field(h.txn_type()),
            csv_field(&h.source_table),
            h.txn_date_raw.map(|d| d.to_string()).unwrap_or_default(),
            h.counter.map(|c| c.to_string()).unwrap_or_default(),
            h.date_source.as_str(),
            h.page_number,
            h.page_offset,
        )?;
    }

    // lineitems.csv
    let mut lif: Vec<u8> = Vec::new();
    writeln!(
        lif,
        "invoice_id,item_qb_id,value,value_undecoded,value_raw_hex,txn_date,date_source,flags,source_table,page_number,page_offset"
    )?;
    for li in &items {
        let value = li.value.map(|v| v.to_string()).unwrap_or_default();
        writeln!(
            lif,
            "{},{},{},{},{},{},{},{},{},{},{}",
            csv_field(&li.invoice_id),
            csv_field(li.item_qb_id.as_deref().unwrap_or("")),
            value,
            li.value_undecoded.map_or("", |r| r.as_str()),
            hex(&li.value_raw),
            lineitem_date(li),
            li.date_source.as_str(),
            csv_field(&flags_csv(li)),
            csv_field(li.source_table.as_deref().unwrap_or("")),
            li.page_number,
            li.page_offset,
        )?;
    }

    // Always written, so an empty list is distinguishable from a missing file.
    let mut rf: Vec<u8> = Vec::new();
    writeln!(rf, "page_number,page_offset,kind,message")?;
    for e in &rejected {
        writeln!(
            rf,
            "{},{},{},{}",
            e.page_number().map(|v| v.to_string()).unwrap_or_default(),
            e.page_offset().map(|v| v.to_string()).unwrap_or_default(),
            e.kind().as_str(),
            csv_field(&e.to_string()),
        )?;
    }
    let manifest = csv_manifest(
        &[
            ("catalog.csv", tables.len()),
            ("transactions.csv", headers.len()),
            ("lineitems.csv", items.len()),
            ("rejected.csv", rejected.len()),
        ],
        &counts,
    );

    // Everything is staged before anything is replaced.
    std::fs::create_dir_all(&out).with_context(|| format!("creating {:?}", out))?;
    let mut set = StagedSet::default();
    for (name, bytes) in [
        ("catalog.csv", cat.as_slice()),
        ("transactions.csv", txf.as_slice()),
        ("lineitems.csv", lif.as_slice()),
        ("rejected.csv", rf.as_slice()),
        ("manifest.json", manifest.as_bytes()),
    ] {
        set.add(&out.join(name), bytes)?;
    }
    set.commit()?;

    println!(
        "openqbw migrate csv: out={:?} tables={} transactions={} lineitems={} undecoded_values={} skipped_records={} duplicate_headers={}",
        out,
        tables.len(),
        headers.len(),
        items.len(),
        items.iter().filter(|li| li.value.is_none()).count(),
        rejected.len(),
        header_dupes.duplicates,
    );
    Ok(())
}

/// `manifest.json` for a CSV export: the format version, the writer's
/// version, and every file written with its data-row count. File names are
/// fixed ASCII, so no JSON escaping is needed.
fn csv_manifest(files: &[(&str, usize)], counts: &RunCounts) -> String {
    let counts = counts
        .iter()
        .map(|(name, n)| format!("    \"{name}\": {n}"))
        .collect::<Vec<_>>()
        .join(",\n");
    let files = files
        .iter()
        .map(|(name, rows)| format!("    \"{name}\": {{\"rows\": {rows}}}"))
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"format_version\": {CSV_FORMAT_VERSION},\n  \"openqbw_version\": \"{}\",\n  \"source_table_attribution\": \"{SOURCE_TABLE_ATTRIBUTION}\",\n  \"files\": {{\n{files}\n  }},\n  \"counts\": {{\n{counts}\n  }}\n}}\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn run_migrate_iif(
    input: PathBuf,
    out: PathBuf,
    placeholder_accounts: bool,
    lenient: bool,
) -> Result<()> {
    if !placeholder_accounts {
        anyhow::bail!(
            "IIF export is not available: account names and transaction types are not decoded \
             from the file, so the output would post entries to invented accounts. Pass \
             --placeholder-accounts to write it anyway (every TRNS uses the account \
             'Accounts Receivable', every SPL uses the internal source-table name as its \
             account, parents without a header become GENERAL JOURNAL, and lines with an \
             undecoded value are left out and counted). The value is the line's numeric \
             value field; it is not established as the line amount."
        );
    }
    outfile::check_output(&input, &out)?;
    let reader = open_reader(&input, lenient)?;
    let Records {
        items,
        rejected,
        headers,
        header_dupes,
        ..
    } = collect_records(&reader)?;
    let (text, stats) = render_iif(&items, &headers);
    let staged = Staged::new(&out);
    staged.write(text.as_bytes())?;
    staged.commit()?;
    println!(
        "openqbw migrate iif: out={:?} transactions={} lineitems={} skipped_transactions={} \
         omitted_lineitems={} (undecoded values) skipped_records={} duplicate_headers={}",
        out,
        stats.written_txns,
        stats.written_lines,
        stats.skipped_txns,
        stats.omitted_lines,
        rejected.len(),
        header_dupes.duplicates,
    );
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct IifStats {
    written_txns: usize,
    written_lines: usize,
    /// Transactions with no decoded line, left out whole.
    skipped_txns: usize,
    /// Lines left out because their value is not decoded.
    omitted_lines: usize,
}

/// Render the placeholder-account IIF. A line whose value is not decoded is
/// left out and counted; the `TRNS` amount is the sum of the lines written,
/// so a transaction with an omitted line is partial. A transaction with no
/// decoded line is left out whole.
fn render_iif(items: &[LineItem], headers: &[TransactionHeader]) -> (String, IifStats) {
    use std::fmt::Write;
    // IIF requires CRLF line endings for maximum compatibility with the
    // Windows tools that consume it.
    let nl = "\r\n";
    let mut f = String::new();
    let _ = write!(
        f,
        "!TRNS\tTRNSID\tTRNSTYPE\tDATE\tACCNT\tNAME\tAMOUNT\tMEMO{nl}"
    );
    let _ = write!(
        f,
        "!SPL\tSPLID\tTRNSTYPE\tDATE\tACCNT\tNAME\tAMOUNT\tMEMO{nl}"
    );
    let _ = write!(f, "!ENDTRNS{nl}");

    let mut by_parent: HashMap<&str, Vec<&LineItem>> = HashMap::new();
    for li in items {
        by_parent
            .entry(li.invoice_id.as_str())
            .or_default()
            .push(li);
    }
    // The first header in page order wins, as in every other format.
    let mut header_by_id: HashMap<&str, &TransactionHeader> = HashMap::new();
    for h in headers {
        header_by_id.entry(h.qb_id.as_str()).or_insert(h);
    }
    let mut parent_ids: Vec<&str> = by_parent.keys().copied().collect();
    parent_ids.sort_unstable();

    let mut trns_id: u64 = 1;
    let mut spl_id: u64 = 1;
    let mut stats = IifStats::default();

    for parent_id in parent_ids {
        let kids = &by_parent[parent_id];
        let decoded: Vec<(&LineItem, Decimal)> = kids
            .iter()
            .filter_map(|li| li.value.map(|v| (*li, v)))
            .collect();
        stats.omitted_lines += kids.len() - decoded.len();
        if decoded.is_empty() {
            stats.skipped_txns += 1;
            continue;
        }
        // A sum or negation that does not fit leaves the transaction out
        // whole and counted; it needs values near 10^38.
        let (Some(sum), Some(negated)) = (
            Decimal::checked_sum(decoded.iter().map(|(_, v)| *v)),
            decoded
                .iter()
                .map(|(_, v)| v.checked_neg())
                .collect::<Option<Vec<_>>>(),
        ) else {
            stats.skipped_txns += 1;
            stats.omitted_lines += decoded.len();
            continue;
        };
        let date = kids
            .iter()
            .find_map(|li| li.txn_date_days_since_unix())
            .map(iso_date_from_unix_days)
            .unwrap_or_default();
        let ttype = header_by_id
            .get(parent_id)
            .map(|h| h.txn_type().to_uppercase())
            .unwrap_or_else(|| "GENERAL JOURNAL".to_string());
        let amt = sum;
        let _ = write!(
            f,
            "TRNS\t{trns_id}\t{ttype}\t{date}\tAccounts Receivable\t\t{amt}\tqb_id={parent_id}{nl}"
        );
        for ((li, _), amt) in decoded.iter().zip(&negated) {
            let date = lineitem_date(li);
            let accnt = li.source_table.as_deref().unwrap_or("");
            let memo = li.item_qb_id.as_deref().unwrap_or("");
            let _ = write!(
                f,
                "SPL\t{spl_id}\t{ttype}\t{date}\t{accnt}\t\t{amt}\t{memo}{nl}"
            );
            spl_id += 1;
            stats.written_lines += 1;
        }
        let _ = write!(f, "ENDTRNS{nl}");
        trns_id += 1;
        stats.written_txns += 1;
    }
    (f, stats)
}

// =========================================================================
// forensics -- file-level discovery
// =========================================================================

fn run_forensics(input: PathBuf, lenient: bool) -> Result<()> {
    let reader = open_reader(&input, lenient)?;

    let file_size = reader.file_size();
    let page_count = reader.page_count();

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
        diagnostics::learned_block_count(&reader),
        page_count.div_ceil(16),
        100.0 * diagnostics::learned_block_count(&reader) as f64
            / page_count.div_ceil(16).max(1) as f64,
    );

    let tables = &reader.catalog().items;
    let user_tables: Vec<&SysTableEntry> =
        tables.iter().filter(|t| !is_system_name(&t.name)).collect();
    println!();
    println!("=== catalog ===");
    println!("tables total: {}", tables.len());
    println!("tables user : {}", user_tables.len());

    let Records {
        items,
        rejected,
        headers,
        ..
    } = collect_records(&reader)?;

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
    let decoded_sum = Decimal::checked_sum(items.iter().filter_map(|li| li.value));
    let undecoded = items.iter().filter(|li| li.value.is_none()).count();

    println!("distinct parent invoice ids : {}", parent_ids.len());
    println!("orphan parents (no header)  : {}", orphan_parents);
    println!("childless headers           : {}", childless_headers);
    println!(
        "sum of decoded values       : {} over {} line items (a sum of values, not invoice totals); {} line items have undecoded values and are not included",
        decoded_sum.map_or("overflow".to_owned(), |d| d.to_string()),
        items.len() - undecoded,
        undecoded,
    );
    println!("records skipped (--lenient) : {}", rejected.len());

    if orphan_parents > 0 || childless_headers > 0 {
        println!();
        println!(
            "note: a non-zero orphan count is a discovery signal -- the file \n      may contain partially-purged records or a parent table this \n      build does not parse yet."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use openqbw::Error;

    #[test]
    fn scan_rejections_are_stored_with_their_kind() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let rejected = [Error::Io(std::io::Error::other("disk gone"))];
        let tx = conn.unchecked_transaction().unwrap();
        insert_scan_rejections(&tx, &rejected).unwrap();
        tx.commit().unwrap();
        let (page, offset, kind, message): (Option<i64>, Option<i64>, String, String) = conn
            .query_row(
                "SELECT page_number, page_offset, kind, message FROM scan_rejections",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((page, offset), (None, None));
        assert_eq!(kind, "io");
        assert!(message.contains("disk gone"), "{message}");
    }

    use super::*;

    /// A line of `parent` whose value is `mantissa / 10^scale`, or undecoded.
    fn item(parent: &str, value: Option<(i128, u32)>) -> LineItem {
        LineItem::synthetic(
            parent,
            Some("abmc_invoice_inventory_lineitem"),
            value.map(|(m, s)| Decimal::new(m, s)),
        )
    }

    fn export_db(version: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute(
            "UPDATE meta SET value = ? WHERE key = 'format_version'",
            params![version.to_string()],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
        conn
    }

    #[test]
    fn current_export_format_is_accepted() {
        check_export_format(&export_db(SQLITE_FORMAT_VERSION)).unwrap();
    }

    #[test]
    fn future_export_format_is_rejected() {
        let e = check_export_format(&export_db(SQLITE_FORMAT_VERSION + 1)).unwrap_err();
        assert!(
            e.to_string().contains("newer than this openqbw reads"),
            "{e}"
        );
    }

    #[test]
    fn meta_read_error_is_reported_not_treated_as_missing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT)").unwrap();
        let e = check_export_format(&conn).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("reading meta.format_version"), "{msg}");
        assert!(!msg.contains("not an openqbw export"), "{msg}");
    }

    #[test]
    fn sqlite_without_meta_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(check_export_format(&conn).is_err());
    }

    #[test]
    fn duplicate_header_ids_are_counted() {
        // S3-26: the same qb_id under two header tables kept the first and
        // dropped the conflict without a count.
        let headers = [
            TransactionHeader::synthetic("0000000000001QBm", "abmc_invoice_header"),
            TransactionHeader::synthetic("0000000000001QBm", "abmc_invoice_header"),
            TransactionHeader::synthetic("0000000000001QBm", "abmc_bill_header"),
            TransactionHeader::synthetic("0000000000002QBm", "abmc_bill_header"),
        ];
        let (unique, dupes) = dedupe_headers(headers.to_vec());
        assert_eq!(unique.len(), 2);
        assert_eq!(
            dupes,
            HeaderDupes {
                duplicates: 2,
                conflicting_types: 1
            }
        );
    }

    #[test]
    fn iif_keeps_signed_values_exactly() {
        let items = [item("P1", Some((7547, 2))), item("P1", Some((-7547, 2)))];
        let (text, stats) = render_iif(&items, &[]);
        assert_eq!(stats.written_txns, 1);
        assert!(text.contains("\tAccounts Receivable\t\t0\t"), "{text}");
        assert!(text.contains("\t-75.47\t"), "{text}");
        assert!(text.contains("\t75.47\t"), "{text}");
    }

    #[test]
    fn iif_omits_lines_with_undecoded_values_and_counts_them() {
        let items = [
            item("P1", Some((100, 0))),
            item("P1", None),
            item("P2", Some((200, 0))),
            item("P3", None),
        ];
        let (text, stats) = render_iif(&items, &[]);
        assert_eq!(
            stats,
            IifStats {
                written_txns: 2,
                written_lines: 2,
                skipped_txns: 1,
                omitted_lines: 2
            }
        );
        assert!(text.contains("qb_id=P1"), "{text}");
        assert!(text.contains("qb_id=P2"), "{text}");
        assert!(!text.contains("qb_id=P3"), "{text}");
    }

    #[test]
    fn balance_counts_decoded_groups_of_three_or_more() {
        let items = [
            // Balances.
            item("A", Some((300, 0))),
            item("A", Some((-12550, 2))),
            item("A", Some((-17450, 2))),
            // Two lines: not counted.
            item("B", Some((1, 0))),
            item("B", Some((-1, 0))),
            // Three lines, one undecoded: not counted.
            item("C", Some((1, 0))),
            item("C", Some((-1, 0))),
            item("C", None),
            // Decoded and not zero.
            item("D", Some((1, 0))),
            item("D", Some((1, 0))),
            item("D", Some((1, 0))),
        ];
        assert_eq!(
            balance(&items),
            Balance {
                groups: 2,
                balanced: 1
            }
        );
    }

    #[test]
    fn undecoded_values_are_counted_by_reason() {
        let mut a = item("A", None);
        a.value_undecoded = Some(openqbw::UndecodedReason::DigitOver99);
        let items = [item("A", None), a, item("A", Some((1, 0)))];
        let c = UndecodedCounts::of(&items);
        assert_eq!(
            c,
            UndecodedCounts {
                zero_length: 1,
                digit_over_99: 1,
                exponent_out_of_range: 0
            }
        );
        assert_eq!(c.total(), 2);
    }
}
