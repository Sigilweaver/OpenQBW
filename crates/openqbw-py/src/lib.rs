//! Python bindings for the `openqbw` crate.
//!
//! Exposes a small read-only API for opening a QBW file and iterating
//! over its catalog, line items, transaction headers, and indexes.

#![allow(clippy::useless_conversion)] // pyo3 macro expansions can trigger this on newer clippy

use openqbw_rs::{
    collect_unique as collect_unique_systable, collect_unique_sysindex, scan_lineitems,
    scan_transaction_headers, PageAttribution, ScanOptions, ScanWarning, SysIndexEntry,
    SysTableEntry,
};
use opensqlany::{ApModel, PageStore};
use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use std::path::PathBuf;
use std::sync::Arc;

pyo3::create_exception!(
    openqbw,
    OpenQBWError,
    pyo3::exceptions::PyException,
    "Base class for errors raised by openqbw."
);
pyo3::create_exception!(
    openqbw,
    FormatError,
    OpenQBWError,
    "The file is not a supported QuickBooks company file or a page is damaged."
);
pyo3::create_exception!(
    openqbw,
    RecordError,
    OpenQBWError,
    "A record could not be decoded with confidence. Pass lenient=True to skip such records."
);
pyo3::create_exception!(
    openqbw,
    ContentError,
    OpenQBWError,
    "The file content is not readable (for example password protected)."
);

/// Map a crate error to the Python exception hierarchy. The exception
/// carries `kind`, `page_number` and `page_offset` attributes.
fn to_py_err(py: Python<'_>, e: &openqbw_rs::Error) -> PyErr {
    use openqbw_rs::ErrorKind as K;
    let msg = e.to_string();
    let err = match e.kind() {
        K::Io => return PyIOError::new_err(msg),
        K::BadMagic | K::UnsupportedVersion | K::BadCrc => FormatError::new_err(msg),
        K::UnknownAmountType | K::TruncatedAmount => RecordError::new_err(msg),
        K::ContentOpaque => ContentError::new_err(msg),
        _ => OpenQBWError::new_err(msg),
    };
    let v = err.value(py);
    // Best effort: attributes are conveniences; the message carries the same facts.
    let _ = v.setattr("kind", e.kind().as_str());
    let _ = v.setattr("page_number", e.page_number());
    let _ = v.setattr("page_offset", e.page_offset());
    err
}

fn warn_user(py: Python<'_>, msg: &str) -> PyResult<()> {
    PyErr::warn(
        py,
        &py.get_type::<pyo3::exceptions::PyUserWarning>(),
        &std::ffi::CString::new(msg).map_err(|e| PyValueError::new_err(e.to_string()))?,
        1,
    )
}

/// Split warnings into rejected records and informational warnings, issue
/// Python warnings for them, and build the report.
fn make_report(
    py: Python<'_>,
    items: Vec<Py<PyDict>>,
    scan_warnings: Vec<ScanWarning>,
) -> PyResult<ScanReport> {
    let mut rejected = Vec::new();
    let mut warnings = Vec::new();
    for w in &scan_warnings {
        match w {
            ScanWarning::RecordSkipped(e) => rejected.push(Rejected {
                kind: e.kind().as_str().to_owned(),
                page_number: e.page_number(),
                page_offset: e.page_offset(),
                message: e.to_string(),
            }),
            other => warnings.push(other.to_string()),
        }
    }
    if !rejected.is_empty() {
        let first: Vec<&str> = rejected
            .iter()
            .take(3)
            .map(|r| r.message.as_str())
            .collect();
        warn_user(
            py,
            &format!(
                "openqbw: skipped {} undecodable record(s); first: {}",
                rejected.len(),
                first.join("; ")
            ),
        )?;
    }
    for w in &warnings {
        warn_user(py, &format!("openqbw: {w}"))?;
    }
    Ok(ScanReport {
        items,
        rejected,
        warnings,
    })
}

/// A record a lenient scan skipped.
#[pyclass(module = "openqbw", frozen, get_all)]
#[derive(Clone)]
struct Rejected {
    /// Error class, for example `"unknown_amount_type"`.
    kind: String,
    page_number: Option<u64>,
    page_offset: Option<usize>,
    message: String,
}

#[pymethods]
impl Rejected {
    fn __repr__(&self) -> String {
        format!("<openqbw.Rejected {}>", self.message)
    }
}

/// Result of `Reader.line_items()` and `Reader.transactions()`: `items`, `rejected` and `warnings`.
/// Iterating or taking `len()` goes over `items`.
#[pyclass(module = "openqbw", frozen)]
struct ScanReport {
    items: Vec<Py<PyDict>>,
    rejected: Vec<Rejected>,
    warnings: Vec<String>,
}

#[pymethods]
impl ScanReport {
    /// Decoded line items, as dicts.
    #[getter]
    fn items<'py>(&self, py: Python<'py>) -> Vec<Bound<'py, PyDict>> {
        self.items.iter().map(|d| d.bind(py).clone()).collect()
    }

    /// Records skipped in lenient mode. Always empty in strict mode.
    #[getter]
    fn rejected(&self) -> Vec<Rejected> {
        self.rejected.clone()
    }

    /// Informational warnings, in both modes.
    #[getter]
    fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::PyAny>> {
        let list = pyo3::types::PyList::new(py, self.items(py))?;
        list.call_method0("__iter__")
    }
}

/// A QBW reader. Construct with `openqbw.open(path)`.
#[pyclass(module = "openqbw", frozen)]
struct Reader {
    path: PathBuf,
    store: Arc<PageStore>,
    model: Arc<ApModel>,
    attribution: Arc<PageAttribution>,
}

#[pymethods]
impl Reader {
    /// The filesystem path the reader was opened from.
    #[getter]
    fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Number of pages in the underlying page-store.
    #[getter]
    fn page_count(&self) -> u64 {
        self.store.page_count()
    }

    /// File size in bytes.
    #[getter]
    fn file_size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    fn __repr__(&self) -> String {
        format!(
            "<openqbw.Reader path={:?} pages={}>",
            self.path,
            self.store.page_count()
        )
    }

    /// Return the SYSTABLE catalog as a list of dicts.
    fn tables<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let entries: Vec<SysTableEntry> = collect_unique_systable(&self.store, &self.model);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let d = PyDict::new(py);
            d.set_item("table_id", e.table_id)?;
            d.set_item("name", e.name)?;
            d.set_item("col_count", e.col_count)?;
            d.set_item("data_root_page", e.data_root_page)?;
            d.set_item("last_page", e.last_page)?;
            d.set_item("page_number", e.page_number)?;
            out.push(d);
        }
        Ok(out)
    }

    /// Return SYSINDEX entries as a list of dicts.
    fn indexes<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let entries: Vec<SysIndexEntry> = collect_unique_sysindex(&self.store, &self.model);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let d = PyDict::new(py);
            d.set_item("name", e.name)?;
            d.set_item("table_id", e.table_id)?;
            d.set_item("root_page", e.root_page)?;
            d.set_item("page_number", e.page_number)?;
            out.push(d);
        }
        Ok(out)
    }

    /// Scan all line items and return a `ScanReport` result.
    ///
    /// `result.items` is a list of dicts. Each carries the parent
    /// `invoice_id`, the amount fields, the source-table name, the page
    /// coordinates and `flags` (reasons the record deserves less trust).
    /// `amount_cents` is in cents under `amount_encoding`; `None` means the
    /// amount is not decoded, not zero. `amount_raw` holds the 4 undecoded
    /// amount bytes.
    ///
    /// A record that cannot be decoded raises `RecordError`. With
    /// `lenient=True` such records are skipped, listed in
    /// `result.rejected`, and a `UserWarning` is issued; informational
    /// warnings are in `result.warnings` in both modes.
    #[pyo3(signature = (*, lenient=false))]
    fn line_items(&self, py: Python<'_>, lenient: bool) -> PyResult<ScanReport> {
        let options = if lenient {
            ScanOptions::lenient()
        } else {
            ScanOptions::strict()
        };
        let scan = scan_lineitems(&self.store, &self.model, Some(&self.attribution), &options)
            .map_err(|e| to_py_err(py, &e))?;
        let mut items = Vec::with_capacity(scan.items.len());
        for li in scan.items {
            let d = PyDict::new(py);
            let date = li.txn_date_days_since_unix();
            d.set_item("invoice_id", li.invoice_id)?;
            d.set_item("item_qb_id", li.item_qb_id)?;
            d.set_item("amount_cents", li.amount_cents)?;
            d.set_item("amount_encoding", li.amount_encoding.as_str())?;
            d.set_item("amount_type", li.amount_raw[0])?;
            d.set_item("amount_raw", PyBytes::new(py, &li.amount_raw))?;
            d.set_item("txn_date_raw", li.txn_date_raw)?;
            d.set_item("counter", li.counter)?;
            d.set_item("date_source", li.date_source.as_str())?;
            d.set_item("txn_date_days_since_unix", date)?;
            d.set_item("source_table", li.source_table)?;
            d.set_item("page_number", li.page_number)?;
            d.set_item("page_offset", li.page_offset)?;
            let flags: Vec<&str> = li.flags.iter().map(|f| f.as_str()).collect();
            d.set_item("flags", flags)?;
            items.push(d.unbind());
        }
        make_report(py, items, scan.warnings)
    }

    /// Scan all transaction headers and return a `ScanReport` like
    /// `line_items()`. Items carry `qb_id`, `source_table`, `txn_type`,
    /// `txn_date_raw`, `counter`, `date_source`, `flags` and the page
    /// coordinates.
    #[pyo3(signature = (*, lenient=false))]
    fn transactions(&self, py: Python<'_>, lenient: bool) -> PyResult<ScanReport> {
        let options = if lenient {
            ScanOptions::lenient()
        } else {
            ScanOptions::strict()
        };
        let scan = scan_transaction_headers(&self.store, &self.model, &self.attribution, &options)
            .map_err(|e| to_py_err(py, &e))?;
        let mut items = Vec::with_capacity(scan.items.len());
        for h in scan.items {
            let d = PyDict::new(py);
            d.set_item("qb_id", &h.qb_id)?;
            d.set_item("source_table", &h.source_table)?;
            d.set_item("txn_type", h.txn_type().to_string())?;
            d.set_item("txn_date_raw", h.txn_date_raw)?;
            d.set_item("counter", h.counter)?;
            d.set_item("date_source", h.date_source.as_str())?;
            let flags: Vec<&str> = h.flags.iter().map(|f| f.as_str()).collect();
            d.set_item("flags", flags)?;
            d.set_item("page_number", h.page_number)?;
            d.set_item("page_offset", h.page_offset)?;
            items.push(d.unbind());
        }
        make_report(py, items, scan.warnings)
    }
}

/// Open a QBW file and return a `Reader`.
#[pyfunction]
fn open(path: PathBuf) -> PyResult<Reader> {
    let store = PageStore::open(&path)
        .map_err(|e| PyIOError::new_err(format!("opening {:?}: {e}", path)))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);
    Ok(Reader {
        path,
        store: Arc::new(store),
        model: Arc::new(model),
        attribution: Arc::new(attribution),
    })
}

/// The `openqbw` Python extension module.
#[pymodule]
fn openqbw(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Reader>()?;
    m.add_class::<ScanReport>()?;
    m.add_class::<Rejected>()?;
    m.add("OpenQBWError", m.py().get_type::<OpenQBWError>())?;
    m.add("FormatError", m.py().get_type::<FormatError>())?;
    m.add("RecordError", m.py().get_type::<RecordError>())?;
    m.add("ContentError", m.py().get_type::<ContentError>())?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    Ok(())
}
