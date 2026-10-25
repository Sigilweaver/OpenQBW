//! Python bindings for the `openqbw` crate.
//!
//! The native module is `openqbw._openqbw`; the `openqbw` package
//! re-exports it together with the exception classes defined in
//! `openqbw/_errors.py`.

#![allow(clippy::useless_conversion)] // pyo3 macro expansions can trigger this on newer clippy

use std::path::PathBuf;

use openqbw_rs::{
    ErrorKind, LineItem as RsLineItem, ReadOptions, Scan, SysIndexEntry, SysTableEntry,
    TransactionHeader,
};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

/// Look up a class defined in `openqbw._errors`.
fn errors_attr<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import("openqbw._errors")?.getattr(name)
}

/// Map a crate error to the Python exception hierarchy. The exception
/// carries `kind`, `page_number` and `page_offset` attributes.
fn to_py_err(py: Python<'_>, e: &openqbw_rs::Error, path: Option<&PathBuf>) -> PyErr {
    match build_py_err(py, e, path) {
        Ok(err) => err,
        Err(err) => err,
    }
}

fn build_py_err(py: Python<'_>, e: &openqbw_rs::Error, path: Option<&PathBuf>) -> PyResult<PyErr> {
    let class = match e.kind() {
        ErrorKind::Io => "OpenQBWIOError",
        ErrorKind::UnknownAmountType | ErrorKind::TruncatedAmount => "RecordError",
        ErrorKind::ContentOpaque => "ContentError",
        ErrorKind::BadMagic
        | ErrorKind::UnsupportedVersion
        | ErrorKind::UnsupportedFlags
        | ErrorKind::BadCrc
        | ErrorKind::BadTrailer
        | ErrorKind::Store => "FormatError",
        _ => "OpenQBWError",
    };
    let cls = errors_attr(py, class)?;
    let value = match (e, path) {
        (openqbw_rs::Error::Io(io), Some(p)) if io.raw_os_error().is_some() => {
            cls.call1((io.raw_os_error(), io.to_string(), p.display().to_string()))?
        }
        _ => cls.call1((e.to_string(),))?,
    };
    value.setattr("kind", e.kind().as_str())?;
    value.setattr("page_number", e.page_number())?;
    value.setattr("page_offset", e.page_offset())?;
    Ok(PyErr::from_value(value))
}

fn warn(py: Python<'_>, msg: &str) -> PyResult<()> {
    let category = errors_attr(py, "OpenQBWWarning")?;
    py.import("warnings")?
        .call_method1("warn", (msg, category, 2))?;
    Ok(())
}

/// A record or page a lenient read skipped.
#[pyclass(module = "openqbw", frozen, get_all)]
#[derive(Clone)]
struct Rejected {
    /// Stable error code, for example `"unknown_amount_type"`.
    kind: String,
    page_number: Option<u64>,
    page_offset: Option<usize>,
    message: String,
}

#[pymethods]
impl Rejected {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("kind", &self.kind)?;
        d.set_item("page_number", self.page_number)?;
        d.set_item("page_offset", self.page_offset)?;
        d.set_item("message", &self.message)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!("<openqbw.Rejected {}: {}>", self.kind, self.message)
    }
}

/// A condition that applies to a whole read.
#[pyclass(module = "openqbw", name = "ReadWarning", frozen, get_all)]
#[derive(Clone)]
struct ReadWarning {
    /// Stable warning code, for example `"opaque_pages"`.
    kind: String,
    message: String,
}

#[pymethods]
impl ReadWarning {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("kind", &self.kind)?;
        d.set_item("message", &self.message)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!("<openqbw.ReadWarning {}: {}>", self.kind, self.message)
    }
}

/// Records found, records a lenient read skipped (`rejected`), and
/// conditions that apply to the whole read (`warnings`). Iterating,
/// indexing and `len()` go over `items`.
#[pyclass(module = "openqbw", frozen)]
struct ScanReport {
    items: Py<PyList>,
    rejected: Vec<Rejected>,
    warnings: Vec<ReadWarning>,
}

#[pymethods]
impl ScanReport {
    /// The records, in page order.
    #[getter]
    fn items<'py>(&self, py: Python<'py>) -> Bound<'py, PyList> {
        self.items.bind(py).clone()
    }

    /// Records or pages skipped by a lenient read. Always empty in strict
    /// mode.
    #[getter]
    fn rejected(&self) -> Vec<Rejected> {
        self.rejected.clone()
    }

    /// Conditions that apply to the whole read, in both modes.
    #[getter]
    fn warnings(&self) -> Vec<ReadWarning> {
        self.warnings.clone()
    }

    fn __len__(&self, py: Python<'_>) -> usize {
        self.items.bind(py).len()
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.items.bind(py).call_method0("__iter__")
    }

    fn __getitem__<'py>(
        &self,
        py: Python<'py>,
        index: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.items.bind(py).as_any().get_item(index)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "<openqbw.ScanReport items={} rejected={} warnings={}>",
            self.items.bind(py).len(),
            self.rejected.len(),
            self.warnings.len()
        )
    }
}

/// Build a report, issuing an `OpenQBWWarning` for the skipped records and
/// for each run-level warning.
fn report<T, P>(py: Python<'_>, scan: &Scan<T>, to_py: impl Fn(&T) -> P) -> PyResult<ScanReport>
where
    P: pyo3::PyClass + Into<pyo3::PyClassInitializer<P>>,
{
    let items = PyList::empty(py);
    for t in &scan.items {
        items.append(Py::new(py, to_py(t))?)?;
    }
    let rejected: Vec<Rejected> = scan
        .rejected
        .iter()
        .map(|e| Rejected {
            kind: e.kind().as_str().to_owned(),
            page_number: e.page_number(),
            page_offset: e.page_offset(),
            message: e.to_string(),
        })
        .collect();
    let warnings: Vec<ReadWarning> = scan
        .warnings
        .iter()
        .map(|w| ReadWarning {
            kind: w.kind.as_str().to_owned(),
            message: w.to_string(),
        })
        .collect();
    if !rejected.is_empty() {
        let first: Vec<&str> = rejected
            .iter()
            .take(3)
            .map(|r| r.message.as_str())
            .collect();
        warn(
            py,
            &format!(
                "openqbw: skipped {} undecodable record(s); first: {}",
                rejected.len(),
                first.join("; ")
            ),
        )?;
    }
    for w in &warnings {
        warn(py, &format!("openqbw: {}", w.message))?;
    }
    Ok(ScanReport {
        items: items.unbind(),
        rejected,
        warnings,
    })
}

fn flag_names(flags: &[openqbw_rs::RecordFlag]) -> Vec<String> {
    flags.iter().map(|f| f.as_str().to_owned()).collect()
}

/// A line item.
#[pyclass(module = "openqbw", frozen, get_all)]
struct LineItem {
    invoice_id: String,
    item_qb_id: Option<String>,
    /// Cents under `amount_encoding`; `None` means not decoded, not zero.
    amount_cents: Option<i64>,
    amount_encoding: String,
    amount_type: u8,
    amount_raw: Py<PyBytes>,
    txn_date_raw: Option<u32>,
    counter: Option<u32>,
    date_source: String,
    txn_date_days_since_unix: Option<i64>,
    source_table: Option<String>,
    page_number: u64,
    page_offset: usize,
    flags: Vec<String>,
}

impl LineItem {
    fn new(py: Python<'_>, li: &RsLineItem) -> Self {
        Self {
            invoice_id: li.invoice_id.clone(),
            item_qb_id: li.item_qb_id.clone(),
            amount_cents: li.amount_cents,
            amount_encoding: li.amount_encoding.as_str().to_owned(),
            amount_type: li.amount_type_byte(),
            amount_raw: PyBytes::new(py, &li.amount_raw).unbind(),
            txn_date_raw: li.txn_date_raw,
            counter: li.counter,
            date_source: li.date_source.as_str().to_owned(),
            txn_date_days_since_unix: li.txn_date_days_since_unix(),
            source_table: li.source_table.clone(),
            page_number: li.page_number,
            page_offset: li.page_offset,
            flags: flag_names(&li.flags),
        }
    }
}

#[pymethods]
impl LineItem {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("invoice_id", &self.invoice_id)?;
        d.set_item("item_qb_id", &self.item_qb_id)?;
        d.set_item("amount_cents", self.amount_cents)?;
        d.set_item("amount_encoding", &self.amount_encoding)?;
        d.set_item("amount_type", self.amount_type)?;
        d.set_item("amount_raw", self.amount_raw.bind(py))?;
        d.set_item("txn_date_raw", self.txn_date_raw)?;
        d.set_item("counter", self.counter)?;
        d.set_item("date_source", &self.date_source)?;
        d.set_item("txn_date_days_since_unix", self.txn_date_days_since_unix)?;
        d.set_item("source_table", &self.source_table)?;
        d.set_item("page_number", self.page_number)?;
        d.set_item("page_offset", self.page_offset)?;
        d.set_item("flags", &self.flags)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "<openqbw.LineItem invoice_id={} amount_cents={:?} source_table={:?}>",
            self.invoice_id, self.amount_cents, self.source_table
        )
    }
}

/// A transaction header.
#[pyclass(module = "openqbw", frozen, get_all)]
struct Transaction {
    qb_id: String,
    source_table: String,
    txn_type: String,
    txn_date_raw: Option<u32>,
    counter: Option<u32>,
    date_source: String,
    txn_date_days_since_unix: Option<i64>,
    page_number: u64,
    page_offset: usize,
    flags: Vec<String>,
}

impl Transaction {
    fn new(h: &TransactionHeader) -> Self {
        Self {
            qb_id: h.qb_id.clone(),
            source_table: h.source_table.clone(),
            txn_type: h.txn_type().to_owned(),
            txn_date_raw: h.txn_date_raw,
            counter: h.counter,
            date_source: h.date_source.as_str().to_owned(),
            txn_date_days_since_unix: h.txn_date_days_since_unix(),
            page_number: h.page_number,
            page_offset: h.page_offset,
            flags: flag_names(&h.flags),
        }
    }
}

#[pymethods]
impl Transaction {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("qb_id", &self.qb_id)?;
        d.set_item("source_table", &self.source_table)?;
        d.set_item("txn_type", &self.txn_type)?;
        d.set_item("txn_date_raw", self.txn_date_raw)?;
        d.set_item("counter", self.counter)?;
        d.set_item("date_source", &self.date_source)?;
        d.set_item("txn_date_days_since_unix", self.txn_date_days_since_unix)?;
        d.set_item("page_number", self.page_number)?;
        d.set_item("page_offset", self.page_offset)?;
        d.set_item("flags", &self.flags)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "<openqbw.Transaction qb_id={} txn_type={}>",
            self.qb_id, self.txn_type
        )
    }
}

/// A `SYSTABLE` catalog row.
#[pyclass(module = "openqbw", frozen, get_all)]
struct Table {
    table_id: u32,
    name: String,
    col_count: Option<u8>,
    data_root_page: Option<u32>,
    last_page: Option<u32>,
    page_number: u64,
    flags: Vec<String>,
}

impl Table {
    fn new(e: &SysTableEntry) -> Self {
        Self {
            table_id: e.table_id,
            name: e.name.clone(),
            col_count: e.col_count,
            data_root_page: e.data_root_page,
            last_page: e.last_page,
            page_number: e.page_number,
            flags: flag_names(&e.flags),
        }
    }
}

#[pymethods]
impl Table {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("table_id", self.table_id)?;
        d.set_item("name", &self.name)?;
        d.set_item("col_count", self.col_count)?;
        d.set_item("data_root_page", self.data_root_page)?;
        d.set_item("last_page", self.last_page)?;
        d.set_item("page_number", self.page_number)?;
        d.set_item("flags", &self.flags)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!("<openqbw.Table {} {}>", self.table_id, self.name)
    }
}

/// A `SYSINDEX` catalog row.
#[pyclass(module = "openqbw", frozen, get_all)]
struct Index {
    name: String,
    table_id: u32,
    root_page: u32,
    is_foreign_key: bool,
    page_number: u64,
    flags: Vec<String>,
}

impl Index {
    fn new(e: &SysIndexEntry) -> Self {
        Self {
            name: e.name.clone(),
            table_id: e.table_id,
            root_page: e.root_page,
            is_foreign_key: e.is_foreign_key(),
            page_number: e.page_number,
            flags: flag_names(&e.flags),
        }
    }
}

#[pymethods]
impl Index {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("name", &self.name)?;
        d.set_item("table_id", self.table_id)?;
        d.set_item("root_page", self.root_page)?;
        d.set_item("is_foreign_key", self.is_foreign_key)?;
        d.set_item("page_number", self.page_number)?;
        d.set_item("flags", &self.flags)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!("<openqbw.Index {} table_id={}>", self.name, self.table_id)
    }
}

/// An open QuickBooks company file. Construct with `openqbw.open(path)`.
#[pyclass(module = "openqbw", frozen)]
struct Reader {
    path: PathBuf,
    inner: openqbw_rs::Reader,
}

#[pymethods]
impl Reader {
    /// The filesystem path the reader was opened from.
    #[getter]
    fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// `True` when the reader skips undecodable records.
    #[getter]
    fn lenient(&self) -> bool {
        !self.inner.options().leniency.is_strict()
    }

    /// Why page-to-table attribution is unavailable (`"no_catalog_rows"` or
    /// `"all_roots_zeroed"`), or `None` when it is available.
    #[getter]
    fn attribution_gap(&self) -> Option<&'static str> {
        self.inner.attribution_gap().map(|g| g.as_str())
    }

    /// Number of 4096-byte pages.
    #[getter]
    fn page_count(&self) -> u64 {
        self.inner.page_count()
    }

    /// File size in bytes.
    #[getter]
    fn file_size(&self) -> u64 {
        self.inner.file_size()
    }

    fn __repr__(&self) -> String {
        format!(
            "<openqbw.Reader path={:?} pages={}>",
            self.path,
            self.inner.page_count()
        )
    }

    /// The `SYSTABLE` catalog, as a `ScanReport` of `Table`.
    fn tables(&self, py: Python<'_>) -> PyResult<ScanReport> {
        report(py, self.inner.catalog(), Table::new)
    }

    /// The `SYSINDEX` catalog, as a `ScanReport` of `Index`.
    fn indexes(&self, py: Python<'_>) -> PyResult<ScanReport> {
        let scan = self.inner.indexes().map_err(|e| to_py_err(py, &e, None))?;
        report(py, &scan, Index::new)
    }

    /// Every line item, as a `ScanReport` of `LineItem`.
    ///
    /// A record that cannot be decoded raises `RecordError` unless the file
    /// was opened with `lenient=True`, in which case it is listed in
    /// `rejected` and an `OpenQBWWarning` is issued.
    fn line_items(&self, py: Python<'_>) -> PyResult<ScanReport> {
        let scan = self
            .inner
            .line_items()
            .map_err(|e| to_py_err(py, &e, None))?;
        report(py, &scan, |li| LineItem::new(py, li))
    }

    /// Every transaction header, as a `ScanReport` of `Transaction`.
    fn transactions(&self, py: Python<'_>) -> PyResult<ScanReport> {
        let scan = self
            .inner
            .transactions()
            .map_err(|e| to_py_err(py, &e, None))?;
        report(py, &scan, Transaction::new)
    }
}

/// Open a QuickBooks company file.
///
/// Raises `OpenQBWIOError` (an `OSError`) when the file cannot be read and
/// `FormatError` when it is not a supported company file or a page is
/// damaged. With `lenient=True`, records and pages that cannot be decoded
/// are skipped and listed in each result's `rejected` instead of raising.
/// Issues an `OpenQBWWarning` for each condition that applies to the whole
/// file, such as unavailable page-to-table attribution.
#[pyfunction]
#[pyo3(signature = (path, *, lenient = false))]
fn open(py: Python<'_>, path: PathBuf, lenient: bool) -> PyResult<Reader> {
    let options = if lenient {
        ReadOptions::lenient()
    } else {
        ReadOptions::strict()
    };
    let inner =
        openqbw_rs::Reader::open(&path, &options).map_err(|e| to_py_err(py, &e, Some(&path)))?;
    for w in &inner.catalog().warnings {
        warn(py, &format!("openqbw: {w}"))?;
    }
    Ok(Reader { path, inner })
}

/// The native part of the `openqbw` package.
#[pymodule]
fn _openqbw(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Reader>()?;
    m.add_class::<ScanReport>()?;
    m.add_class::<Rejected>()?;
    m.add_class::<ReadWarning>()?;
    m.add_class::<LineItem>()?;
    m.add_class::<Transaction>()?;
    m.add_class::<Table>()?;
    m.add_class::<Index>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    Ok(())
}
