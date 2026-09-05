//! ClickHouse readers: a remote server over HTTP, and the embedded chDB engine.
//!
//! Both speak ClickHouse SQL and both exchange data as Arrow IPC streams, so
//! they share one reader implementation, [`ClickHouseSqlReader`], that is
//! generic over a [`Transport`]:
//!
//! - [`ClickHouseReader`] (`clickhouse://`, feature `clickhouse`) sends each
//!   statement to a server's HTTP interface ([`http::HttpTransport`]).
//! - [`ChdbReader`] (`chdb://`, feature `chdb`) runs statements in-process on
//!   libchdb, loaded at runtime ([`chdb::ChdbTransport`]). It also serves as
//!   the in-memory [`CacheBackend`] behind `chdb+<primary>://` connection
//!   strings, so a ClickHouse setup never needs another database engine.
//!
//! # Types
//!
//! ClickHouse's Arrow output cannot express some of its own types: `DateTime`
//! arrives as a bare `UInt32`, `Enum` as its integer codes, `UUID`/`IPv4`/
//! `IPv6`/128- and 256-bit integers as raw bytes, and `Decimal` as an Arrow
//! decimal the plot pipeline does not consume. Before running a `SELECT`, the
//! reader asks the engine for the result's column types (`DESCRIBE TABLE (…)`)
//! and, when any such column is present, wraps the query in
//! `SELECT * REPLACE (…)` that converts those columns server-side
//! (`DateTime` → `DateTime64`, everything else → `String` or `Float64`).
//! Timestamps keep their instant; ggsql renders them in UTC.
//!
//! # Temporary tables
//!
//! The executor materializes CTEs and the global query as temporary tables.
//! Each reader owns one session, so those tables (and `SET` statements) stay
//! visible across the statements of a ggsql query. A server account without
//! the `CREATE TEMPORARY TABLE` privilege is detected on connect
//! ([`ClickHouseSqlReader::supports_temporary_tables`]); the connection
//! factory then keeps intermediate tables in an embedded chDB cache instead.

use std::cell::{OnceCell, RefCell};
use std::collections::HashSet;
use std::io::Cursor;

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;

use crate::array_util::value_to_string;
use crate::reader::{ColumnInfo, Reader, SqlDialect, TableInfo};
use crate::{naming, DataFrame, GgsqlError, Result};

#[cfg(feature = "chdb")]
pub mod chdb;
#[cfg(feature = "clickhouse")]
pub mod http;

#[cfg(feature = "chdb")]
pub use chdb::{ChdbReader, ChdbTransport};
#[cfg(feature = "clickhouse")]
pub use http::{ClickHouseConfig, ClickHouseReader, HttpTransport};

// =============================================================================
// Dialect
// =============================================================================

/// ClickHouse SQL dialect, shared by the HTTP and chDB readers.
///
/// Cast targets are `Nullable(…)` because ClickHouse refuses to cast `NULL` to
/// a non-nullable type; temporary tables need the `TEMPORARY` keyword; and
/// quantiles use the native `quantileExactInclusive`, which matches the
/// linear-interpolation semantics of `QUANTILE_CONT`. The caching layer's memo
/// table is a `Memory` table maintained with synchronous `ALTER TABLE`
/// mutations, since ClickHouse has no `INSERT OR REPLACE`, `UPDATE` or
/// `DELETE FROM` for that engine.
pub struct ClickHouseDialect;

/// ClickHouse string literal: single quotes doubled, backslashes escaped
/// (ClickHouse interprets backslash escapes inside string literals).
pub(crate) fn ch_literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

/// Print `sql` to stderr when `GGSQL_CLICKHOUSE_TRACE` is set, so the
/// statements actually sent (including the `DESCRIBE`/`REPLACE` rewrites) can
/// be inspected.
fn trace_sql(sql: &str) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("GGSQL_CLICKHOUSE_TRACE").is_some()) {
        eprintln!("[clickhouse] {sql}");
    }
}

/// Comma-separated argument list with every expression cast to Float64.
fn float_args(exprs: &[&str]) -> String {
    exprs
        .iter()
        .map(|e| format!("toFloat64({e})"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl SqlDialect for ClickHouseDialect {
    fn number_type_name(&self) -> Option<&str> {
        Some("Nullable(Float64)")
    }

    fn integer_type_name(&self) -> Option<&str> {
        Some("Nullable(Int64)")
    }

    fn date_type_name(&self) -> Option<&str> {
        Some("Nullable(Date32)")
    }

    fn datetime_type_name(&self) -> Option<&str> {
        Some("Nullable(DateTime64(6))")
    }

    /// ClickHouse has no portable time-of-day type; time columns are left as is.
    fn time_type_name(&self) -> Option<&str> {
        None
    }

    fn string_type_name(&self) -> Option<&str> {
        Some("Nullable(String)")
    }

    fn boolean_type_name(&self) -> Option<&str> {
        Some("Nullable(Bool)")
    }

    // The executor only asks for greatest/least of numeric expressions. The
    // arguments are cast to Float64 because ClickHouse has no common supertype
    // for UInt64 (the type of `count()` and `numbers()`) and Float64.

    fn sql_greatest(&self, exprs: &[&str]) -> String {
        if exprs.len() == 1 {
            return exprs[0].to_string();
        }
        format!("greatest({})", float_args(exprs))
    }

    fn sql_least(&self, exprs: &[&str]) -> String {
        if exprs.len() == 1 {
            return exprs[0].to_string();
        }
        format!("least({})", float_args(exprs))
    }

    /// Older ClickHouse versions only accept `IS NOT DISTINCT FROM` inside
    /// `JOIN ON`; this spelling works in any clause on every version.
    fn sql_null_safe_equals(&self, left: &str, right: &str) -> String {
        format!("(({left} = {right}) OR ({left} IS NULL AND {right} IS NULL))")
    }

    fn sql_select_replace(
        &self,
        expr: &str,
        col: &str,
        from: &str,
        _all_columns: &[String],
    ) -> String {
        format!("SELECT * REPLACE ({expr} AS {col}) FROM ({from})")
    }

    fn sql_generate_series(&self, n: usize) -> String {
        format!("\"__ggsql_seq__\"(n) AS (SELECT toFloat64(number) AS n FROM numbers({n}))")
    }

    fn sql_quantile_inline(&self, column: &str, fraction: f64) -> Option<String> {
        Some(format!(
            "quantileExactInclusive({})({})",
            fraction,
            naming::quote_ident(column)
        ))
    }

    /// Every caller embeds this in a `GROUP BY {groups}` query over `from`, so
    /// the native aggregate is equivalent to the correlated scalar subquery
    /// other dialects produce, and it also runs on ClickHouse versions without
    /// correlated-subquery support.
    fn sql_percentile(
        &self,
        column: &str,
        fraction: f64,
        _from: &str,
        _groups: &[String],
    ) -> String {
        format!(
            "quantileExactInclusive({fraction})({})",
            naming::quote_ident(column)
        )
    }

    fn sql_date_literal(&self, days_since_epoch: i32) -> String {
        format!("toDate32({days_since_epoch})")
    }

    fn sql_datetime_literal(&self, microseconds_since_epoch: i64) -> String {
        format!("fromUnixTimestamp64Micro({microseconds_since_epoch})")
    }

    fn create_or_replace_temp_table_sql(
        &self,
        name: &str,
        column_aliases: &[String],
        body_sql: &str,
    ) -> Vec<String> {
        let qname = naming::quote_ident(name);
        let body = super::wrap_with_column_aliases(body_sql, column_aliases);
        vec![
            format!("DROP TEMPORARY TABLE IF EXISTS {qname}"),
            format!("CREATE TEMPORARY TABLE {qname} AS {body}"),
        ]
    }

    fn cache_meta_table_sql(&self, table: &str) -> String {
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\
             cache_key String, sql String, table_name String, \
             fetched_at_epoch_ms Int64, last_accessed_epoch_ms Int64, \
             byte_estimate Int64, row_count Int64) ENGINE = Memory",
            naming::quote_ident(table)
        )
    }

    fn cache_meta_upsert_sql(
        &self,
        table: &str,
        key: &str,
        sql: &str,
        table_name: &str,
        now_ms: i64,
        byte_estimate: i64,
        row_count: i64,
    ) -> Vec<String> {
        let q = naming::quote_ident(table);
        vec![
            format!(
                "ALTER TABLE {q} DELETE WHERE cache_key = {}",
                ch_literal(key)
            ),
            format!(
                "INSERT INTO {q} \
                 (cache_key, sql, table_name, fetched_at_epoch_ms, last_accessed_epoch_ms, \
                  byte_estimate, row_count) \
                 VALUES ({}, {}, {}, {now_ms}, {now_ms}, {byte_estimate}, {row_count})",
                ch_literal(key),
                ch_literal(sql),
                ch_literal(table_name),
            ),
        ]
    }

    fn cache_meta_touch_sql(&self, table: &str, key: &str, now_ms: i64) -> String {
        format!(
            "ALTER TABLE {} UPDATE last_accessed_epoch_ms = {now_ms} WHERE cache_key = {}",
            naming::quote_ident(table),
            ch_literal(key)
        )
    }

    fn cache_meta_delete_sql(&self, table: &str, key: &str) -> String {
        format!(
            "ALTER TABLE {} DELETE WHERE cache_key = {}",
            naming::quote_ident(table),
            ch_literal(key)
        )
    }
}

// =============================================================================
// Transport
// =============================================================================

/// How statements reach a ClickHouse engine.
///
/// Implementations run one statement at a time within a single session, so
/// temporary tables and `SET` statements persist between calls.
pub trait Transport: Send {
    /// Run one statement and return the raw bytes of its output in `format`
    /// (a ClickHouse output format name such as `ArrowStream`). Statements
    /// without a result set return an empty buffer.
    fn run(&self, sql: &str, format: &str) -> Result<Vec<u8>>;

    /// Bulk-load an Arrow IPC stream into the existing table `table`
    /// (already quoted for SQL).
    fn insert_arrow(&self, table: &str, ipc: &[u8]) -> Result<()>;

    /// Whether `CREATE TEMPORARY TABLE` is known to work without probing the
    /// engine. `false` makes the reader check once on first use.
    fn assumes_temporary_tables(&self) -> bool {
        false
    }

    /// Short description of the endpoint for error messages.
    fn endpoint(&self) -> String;
}

// =============================================================================
// Reader
// =============================================================================

/// Reader for any ClickHouse engine reachable through a [`Transport`].
///
/// Use the concrete aliases: [`ClickHouseReader`] for a server, [`ChdbReader`]
/// for the embedded engine.
pub struct ClickHouseSqlReader<T: Transport> {
    transport: T,
    registered_tables: RefCell<HashSet<String>>,
    temp_tables_supported: OnceCell<bool>,
}

impl<T: Transport> ClickHouseSqlReader<T> {
    /// Wrap a transport. Connects eagerly: a `SELECT 1` round trip verifies
    /// the endpoint and credentials so a bad connection string fails here
    /// rather than on the first query.
    pub fn from_transport(transport: T) -> Result<Self> {
        let reader = Self {
            transport,
            registered_tables: RefCell::new(HashSet::new()),
            temp_tables_supported: OnceCell::new(),
        };
        reader.query_arrow("SELECT 1")?;
        Ok(reader)
    }

    /// The underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Whether this session may create temporary tables, which the executor
    /// needs for CTEs and the global query. Probed once (a `CREATE TEMPORARY
    /// TABLE` that is dropped again); a read-only account answers `false`.
    pub fn supports_temporary_tables(&self) -> bool {
        *self.temp_tables_supported.get_or_init(|| {
            if self.transport.assumes_temporary_tables() {
                return true;
            }
            let probe = naming::quote_ident(&format!("__ggsql_probe_{}__", naming::session_id()));
            let ok = self
                .execute_raw(&format!("CREATE TEMPORARY TABLE {probe} (x UInt8)"))
                .is_ok();
            if ok {
                let _ = self.execute_raw(&format!("DROP TEMPORARY TABLE IF EXISTS {probe}"));
            }
            ok
        })
    }

    /// Run a statement that returns no rows.
    fn execute_raw(&self, sql: &str) -> Result<()> {
        trace_sql(sql);
        self.transport.run(sql, "TabSeparated")?;
        Ok(())
    }

    /// Run a row-returning statement as is (no type rewriting) and decode the
    /// Arrow stream.
    fn query_arrow(&self, sql: &str) -> Result<DataFrame> {
        trace_sql(sql);
        decode_arrow_stream(&self.transport.run(sql, "ArrowStream")?)
    }

    /// Column names and ClickHouse type names of a query's result.
    fn describe(&self, sql: &str) -> Result<Vec<(String, String)>> {
        let df = self.query_arrow(&format!("DESCRIBE TABLE ({sql})"))?;
        let names = df.column("name")?;
        let types = df.column("type")?;
        Ok((0..df.height())
            .map(|i| (value_to_string(names, i), value_to_string(types, i)))
            .collect())
    }

    /// Wrap a `SELECT`/`WITH` query so that columns whose ClickHouse type has
    /// no faithful Arrow representation are converted server-side. Any other
    /// statement, or a query the engine cannot describe, is returned unchanged
    /// so the real execution reports the real error.
    fn with_arrow_friendly_types(&self, sql: &str) -> String {
        let first = sql
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if first != "SELECT" && first != "WITH" {
            return sql.to_string();
        }
        let Ok(columns) = self.describe(sql) else {
            return sql.to_string();
        };
        let replacements: Vec<String> = columns
            .iter()
            .filter_map(|(name, ty)| arrow_friendly_expr(name, ty))
            .collect();
        if replacements.is_empty() {
            return sql.to_string();
        }
        format!(
            "SELECT * REPLACE ({}) FROM ({sql})",
            replacements.join(", ")
        )
    }

    fn temporary_table_exists(&self, name: &str) -> Result<bool> {
        let df = self.query_arrow(&format!(
            "EXISTS TEMPORARY TABLE {}",
            naming::quote_ident(name)
        ))?;
        Ok(df.height() == 1 && value_to_string(df.column("result")?, 0) == "1")
    }

    /// Load any `ggsql:<name>` builtin datasets referenced by `sql` into the
    /// session as temporary tables.
    #[cfg(all(feature = "builtin-data", feature = "parquet"))]
    fn ensure_builtin_datasets(&self, sql: &str) -> Result<()> {
        for name in crate::parser::extract_builtin_dataset_names(sql)? {
            let table = naming::builtin_data_table(&name);
            if !self.temporary_table_exists(&table)? {
                let df = super::data::load_builtin_dataframe(&name)?;
                self.register(&table, df, true)?;
            }
        }
        Ok(())
    }
}

impl<T: Transport> Reader for ClickHouseSqlReader<T> {
    fn execute_sql(&self, sql: &str) -> Result<DataFrame> {
        #[cfg(all(feature = "builtin-data", feature = "parquet"))]
        self.ensure_builtin_datasets(sql)?;

        let sql = crate::parser::rewrite_namespaced_sql(sql)?;
        // One statement per call; a trailing terminator is harmless to the
        // engine but would break the `DESCRIBE TABLE (…)` /
        // `SELECT * REPLACE … FROM (…)` wrapping.
        let sql = sql.trim().trim_end_matches(';').trim();

        if !super::returns_rows(sql) {
            self.execute_raw(sql)?;
            return Ok(DataFrame::empty());
        }

        self.query_arrow(&self.with_arrow_friendly_types(sql))
    }

    fn register(&self, name: &str, df: DataFrame, replace: bool) -> Result<()> {
        super::validate_table_name(name)?;
        let qname = naming::quote_ident(name);

        if replace {
            self.execute_raw(&format!("DROP TEMPORARY TABLE IF EXISTS {qname}"))?;
        } else if self.temporary_table_exists(name)? {
            return Err(GgsqlError::ReaderError(format!(
                "Table '{name}' already exists"
            )));
        }

        let schema = df.schema();
        let col_defs: Vec<String> = schema
            .fields()
            .iter()
            .map(|f| {
                format!(
                    "{} {}",
                    naming::quote_ident(f.name()),
                    arrow_type_to_clickhouse(f.data_type(), f.is_nullable())
                )
            })
            .collect();
        self.execute_raw(&format!(
            "CREATE TEMPORARY TABLE {qname} ({})",
            col_defs.join(", ")
        ))
        .map_err(|e| {
            GgsqlError::ReaderError(format!("Failed to create temp table '{name}': {e}"))
        })?;

        if df.height() > 0 {
            let ipc = encode_arrow_stream(df)?;
            self.transport.insert_arrow(&qname, &ipc).map_err(|e| {
                GgsqlError::ReaderError(format!("Failed to insert into '{name}': {e}"))
            })?;
        }

        self.registered_tables.borrow_mut().insert(name.to_string());
        Ok(())
    }

    fn unregister(&self, name: &str) -> Result<()> {
        if !self.registered_tables.borrow().contains(name) {
            return Err(GgsqlError::ReaderError(format!(
                "Table '{name}' was not registered via this reader"
            )));
        }
        self.execute_raw(&format!(
            "DROP TEMPORARY TABLE IF EXISTS {}",
            naming::quote_ident(name)
        ))?;
        self.registered_tables.borrow_mut().remove(name);
        Ok(())
    }

    fn execute(&self, query: &str) -> Result<super::Spec> {
        super::execute_with_reader(self, query)
    }

    fn dialect(&self) -> &dyn SqlDialect {
        &ClickHouseDialect
    }

    // ClickHouse has a single level of namespacing: a database is both the
    // catalog and the schema. `system.*` is more reliable than
    // `information_schema`, which lists every column twice (upper- and
    // lower-case names).

    fn list_catalogs(&self) -> Result<Vec<String>> {
        let df = self.query_arrow(
            "SELECT name FROM system.databases \
             WHERE name NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema') \
             ORDER BY name",
        )?;
        column_strings(&df, "name")
    }

    fn list_schemas(&self, catalog: &str) -> Result<Vec<String>> {
        Ok(vec![catalog.to_string()])
    }

    fn list_tables(&self, _catalog: &str, schema: &str) -> Result<Vec<TableInfo>> {
        let df = self.query_arrow(&format!(
            "SELECT name, \
                    CASE WHEN engine IN ('View', 'MaterializedView', 'LiveView', 'WindowView') \
                         THEN 'VIEW' ELSE 'BASE TABLE' END AS table_type \
             FROM system.tables WHERE database = {} ORDER BY name",
            naming::quote_literal(schema)
        ))?;
        let names = column_strings(&df, "name")?;
        let types = column_strings(&df, "table_type")?;
        Ok(names
            .into_iter()
            .zip(types)
            .map(|(name, table_type)| TableInfo { name, table_type })
            .collect())
    }

    fn list_columns(&self, _catalog: &str, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        let df = self.query_arrow(&format!(
            "SELECT name, type FROM system.columns \
             WHERE database = {} AND table = {} ORDER BY position",
            naming::quote_literal(schema),
            naming::quote_literal(table)
        ))?;
        let names = column_strings(&df, "name")?;
        let types = column_strings(&df, "type")?;
        Ok(names
            .into_iter()
            .zip(types)
            .map(|(name, data_type)| ColumnInfo { name, data_type })
            .collect())
    }
}

// =============================================================================
// Arrow helpers
// =============================================================================

/// Decode an `ArrowStream` payload into a DataFrame. An empty body (a
/// statement without a result set) yields an empty DataFrame; a stream with a
/// schema but no batches keeps its column names and types.
pub(crate) fn decode_arrow_stream(bytes: &[u8]) -> Result<DataFrame> {
    if bytes.is_empty() {
        return Ok(DataFrame::empty());
    }
    let reader = StreamReader::try_new(Cursor::new(bytes), None).map_err(|e| {
        GgsqlError::ReaderError(format!("Failed to decode ClickHouse Arrow stream: {e}"))
    })?;
    let schema = reader.schema();
    let batches = reader
        .collect::<std::result::Result<Vec<RecordBatch>, _>>()
        .map_err(|e| {
            GgsqlError::ReaderError(format!("Failed to decode ClickHouse Arrow batch: {e}"))
        })?;
    let merged = match batches.len() {
        0 => RecordBatch::new_empty(schema),
        1 => batches.into_iter().next().unwrap(),
        _ => arrow::compute::concat_batches(&schema, &batches)
            .map_err(|e| GgsqlError::ReaderError(format!("concat_batches: {e}")))?,
    };
    Ok(DataFrame::from_record_batch(normalize_timestamps(merged)?))
}

/// Cast every timestamp column to microseconds without a time zone, the one
/// timestamp representation the rest of the pipeline works with. ClickHouse
/// emits `DateTime64(p)` in the unit matching `p` and tags it with the server
/// time zone; the instant is preserved, and ggsql renders instants in UTC.
fn normalize_timestamps(batch: RecordBatch) -> Result<RecordBatch> {
    use arrow::datatypes::{Field, Schema};
    use std::sync::Arc;

    let target = DataType::Timestamp(TimeUnit::Microsecond, None);
    if !batch
        .schema()
        .fields()
        .iter()
        .any(|f| matches!(f.data_type(), DataType::Timestamp(_, _)) && f.data_type() != &target)
    {
        return Ok(batch);
    }
    let mut fields = Vec::with_capacity(batch.num_columns());
    let mut columns = Vec::with_capacity(batch.num_columns());
    for (field, column) in batch.schema().fields().iter().zip(batch.columns()) {
        if matches!(field.data_type(), DataType::Timestamp(_, _)) && field.data_type() != &target {
            let cast = arrow::compute::cast(column, &target).map_err(|e| {
                GgsqlError::ReaderError(format!(
                    "Failed to normalize timestamp column '{}': {e}",
                    field.name()
                ))
            })?;
            fields.push(Arc::new(Field::new(
                field.name(),
                target.clone(),
                field.is_nullable(),
            )));
            columns.push(cast);
        } else {
            fields.push(field.clone());
            columns.push(column.clone());
        }
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
        .map_err(|e| GgsqlError::ReaderError(format!("Failed to rebuild record batch: {e}")))
}

/// Encode a DataFrame as an Arrow IPC stream for `INSERT … FORMAT ArrowStream`.
fn encode_arrow_stream(df: DataFrame) -> Result<Vec<u8>> {
    let batch = df.into_inner();
    let mut buf = Vec::new();
    let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())
        .map_err(|e| GgsqlError::ReaderError(format!("Failed to encode Arrow stream: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| GgsqlError::ReaderError(format!("Failed to encode Arrow batch: {e}")))?;
    writer
        .finish()
        .map_err(|e| GgsqlError::ReaderError(format!("Failed to finish Arrow stream: {e}")))?;
    drop(writer);
    Ok(buf)
}

/// The innermost type name of a ClickHouse type, with `Nullable(…)` and
/// `LowCardinality(…)` wrappers and any parameter list removed:
/// `LowCardinality(Nullable(FixedString(16)))` → `FixedString`.
fn base_type_name(ch_type: &str) -> &str {
    let mut t = ch_type.trim();
    loop {
        let inner = ["Nullable(", "LowCardinality("]
            .iter()
            .find_map(|w| t.strip_prefix(w))
            .map(|s| s.strip_suffix(')').unwrap_or(s).trim());
        match inner {
            Some(s) => t = s,
            None => break,
        }
    }
    t.split('(').next().unwrap_or(t).trim()
}

/// A `SELECT * REPLACE` item converting a column to a type ClickHouse can
/// export to Arrow faithfully, or `None` if the column needs no conversion.
fn arrow_friendly_expr(name: &str, ch_type: &str) -> Option<String> {
    let q = naming::quote_ident(name);
    let expr = match base_type_name(ch_type) {
        // Exported as a bare UInt32 otherwise.
        "DateTime" => format!("toDateTime64({q}, 0)"),
        // Exported as integer codes or raw bytes otherwise.
        "Enum" | "Enum8" | "Enum16" | "UUID" | "IPv4" | "IPv6" | "Int128" | "UInt128"
        | "Int256" | "UInt256" | "Time" | "Time64" => format!("toString({q})"),
        // Nested containers have no visual encoding; show their text form.
        "Array" | "Tuple" | "Map" | "Nested" | "Variant" | "Dynamic" | "JSON" | "Object"
        | "Point" | "Ring" | "LineString" | "MultiLineString" | "Polygon" | "MultiPolygon" => {
            format!("toString({q})")
        }
        "FixedString" => format!("toStringCutToZero({q})"),
        "Decimal" | "Decimal32" | "Decimal64" | "Decimal128" | "Decimal256" => {
            format!("toFloat64({q})")
        }
        // The type of a bare NULL literal.
        "Nothing" => format!("CAST({q} AS Nullable(String))"),
        _ => return None,
    };
    Some(format!("{expr} AS {q}"))
}

/// ClickHouse column type for an Arrow field, used when creating the
/// temporary table that receives a registered DataFrame.
fn arrow_type_to_clickhouse(dtype: &DataType, nullable: bool) -> String {
    let base: String = match dtype {
        DataType::Boolean => "Bool".into(),
        DataType::Int8 => "Int8".into(),
        DataType::Int16 => "Int16".into(),
        DataType::Int32 => "Int32".into(),
        DataType::Int64 => "Int64".into(),
        DataType::UInt8 => "UInt8".into(),
        DataType::UInt16 => "UInt16".into(),
        DataType::UInt32 => "UInt32".into(),
        DataType::UInt64 => "UInt64".into(),
        DataType::Float16 | DataType::Float32 => "Float32".into(),
        DataType::Float64 => "Float64".into(),
        DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::FixedSizeBinary(_) => "String".into(),
        DataType::Date32 => "Date32".into(),
        DataType::Date64 => "DateTime64(3)".into(),
        DataType::Timestamp(unit, _) => {
            let precision = match unit {
                TimeUnit::Second => 0,
                TimeUnit::Millisecond => 3,
                TimeUnit::Microsecond => 6,
                TimeUnit::Nanosecond => 9,
            };
            format!("DateTime64({precision})")
        }
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => format!("Decimal({p}, {s})"),
        DataType::Dictionary(_, value) => return arrow_type_to_clickhouse(value, nullable),
        // Containers cannot be Nullable in ClickHouse; nullability lives on
        // the element type.
        DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => {
            return format!(
                "Array({})",
                arrow_type_to_clickhouse(f.data_type(), f.is_nullable())
            )
        }
        DataType::Struct(fields) => {
            let items: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{} {}",
                        naming::quote_ident(f.name()),
                        arrow_type_to_clickhouse(f.data_type(), f.is_nullable())
                    )
                })
                .collect();
            return format!("Tuple({})", items.join(", "));
        }
        DataType::Null => return "Nullable(String)".into(),
        // Durations and times of day have no stable Arrow mapping in
        // ClickHouse; store the raw integer.
        DataType::Time32(_) | DataType::Time64(_) | DataType::Duration(_) => "Int64".into(),
        _ => "String".into(),
    };
    if nullable {
        format!("Nullable({base})")
    } else {
        base
    }
}

/// Decode `%XX` escapes in a connection-string component; anything else is
/// passed through unchanged.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Non-null values of a string column, in row order.
fn column_strings(df: &DataFrame, column: &str) -> Result<Vec<String>> {
    let col = df.column(column)?;
    Ok((0..df.height())
        .filter(|&i| !col.is_null(i))
        .map(|i| value_to_string(col, i))
        .collect())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_type_name() {
        assert_eq!(base_type_name("DateTime"), "DateTime");
        assert_eq!(base_type_name("DateTime('UTC')"), "DateTime");
        assert_eq!(base_type_name("DateTime64(3)"), "DateTime64");
        assert_eq!(base_type_name("Nullable(DateTime)"), "DateTime");
        assert_eq!(
            base_type_name("LowCardinality(Nullable(FixedString(16)))"),
            "FixedString"
        );
        assert_eq!(base_type_name("Enum8('a' = 1, 'b' = 2)"), "Enum8");
        assert_eq!(base_type_name("Array(Nullable(String))"), "Array");
    }

    #[test]
    fn test_arrow_friendly_expr() {
        assert_eq!(
            arrow_friendly_expr("ts", "Nullable(DateTime)").as_deref(),
            Some("toDateTime64(\"ts\", 0) AS \"ts\"")
        );
        assert_eq!(
            arrow_friendly_expr("e", "Enum8('a' = 1)").as_deref(),
            Some("toString(\"e\") AS \"e\"")
        );
        assert_eq!(
            arrow_friendly_expr("d", "Decimal(18, 2)").as_deref(),
            Some("toFloat64(\"d\") AS \"d\"")
        );
        assert_eq!(
            arrow_friendly_expr("f", "FixedString(4)").as_deref(),
            Some("toStringCutToZero(\"f\") AS \"f\"")
        );
        assert_eq!(arrow_friendly_expr("x", "UInt64"), None);
        assert_eq!(arrow_friendly_expr("x", "DateTime64(6, 'UTC')"), None);
        assert_eq!(arrow_friendly_expr("x", "LowCardinality(String)"), None);
        assert_eq!(arrow_friendly_expr("x", "Date"), None);
        assert_eq!(arrow_friendly_expr("x", "Bool"), None);
    }

    #[test]
    fn test_arrow_type_to_clickhouse() {
        assert_eq!(arrow_type_to_clickhouse(&DataType::Int64, false), "Int64");
        assert_eq!(
            arrow_type_to_clickhouse(&DataType::Float64, true),
            "Nullable(Float64)"
        );
        assert_eq!(
            arrow_type_to_clickhouse(&DataType::Utf8, true),
            "Nullable(String)"
        );
        assert_eq!(arrow_type_to_clickhouse(&DataType::Date32, false), "Date32");
        assert_eq!(
            arrow_type_to_clickhouse(&DataType::Timestamp(TimeUnit::Microsecond, None), true),
            "Nullable(DateTime64(6))"
        );
        assert_eq!(arrow_type_to_clickhouse(&DataType::Boolean, false), "Bool");
        let list = DataType::List(std::sync::Arc::new(arrow::datatypes::Field::new(
            "item",
            DataType::Int32,
            true,
        )));
        assert_eq!(
            arrow_type_to_clickhouse(&list, true),
            "Array(Nullable(Int32))",
            "arrays are never Nullable themselves"
        );
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("plain+text"), "plain+text");
        assert_eq!(percent_decode("bad%zz%"), "bad%zz%");
    }

    #[test]
    fn test_dialect_temp_table_sql() {
        let stmts = ClickHouseDialect.create_or_replace_temp_table_sql(
            "__ggsql_t__",
            &["a".to_string(), "b".to_string()],
            "SELECT 1, 2",
        );
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "DROP TEMPORARY TABLE IF EXISTS \"__ggsql_t__\"");
        assert!(stmts[1].starts_with("CREATE TEMPORARY TABLE \"__ggsql_t__\" AS WITH"));
        assert!(stmts[1].contains("\"a\", \"b\""));
    }

    #[test]
    fn test_dialect_quantile_and_series() {
        assert_eq!(
            ClickHouseDialect.sql_quantile_inline("v", 0.25).unwrap(),
            "quantileExactInclusive(0.25)(\"v\")"
        );
        assert_eq!(
            ClickHouseDialect.sql_generate_series(5),
            "\"__ggsql_seq__\"(n) AS (SELECT toFloat64(number) AS n FROM numbers(5))"
        );
        assert_eq!(
            ClickHouseDialect.sql_greatest(&["a", "b"]),
            "greatest(toFloat64(a), toFloat64(b))"
        );
        assert_eq!(ClickHouseDialect.sql_least(&["a"]), "a");
        assert_eq!(
            ClickHouseDialect.sql_percentile("v", 0.5, "ignored", &["g".to_string()]),
            "quantileExactInclusive(0.5)(\"v\")"
        );
        assert_eq!(
            ClickHouseDialect.sql_null_safe_equals("a.k", "b.k"),
            "((a.k = b.k) OR (a.k IS NULL AND b.k IS NULL))"
        );
        assert_eq!(ClickHouseDialect.sql_date_literal(-1), "toDate32(-1)");
        assert_eq!(
            ClickHouseDialect.sql_datetime_literal(1_000_000),
            "fromUnixTimestamp64Micro(1000000)"
        );
        assert_eq!(ClickHouseDialect.time_type_name(), None);
    }

    #[test]
    fn test_dialect_cache_meta_sql() {
        let create = ClickHouseDialect.cache_meta_table_sql("__ggsql_cache_meta__");
        assert!(create.starts_with("CREATE TABLE IF NOT EXISTS \"__ggsql_cache_meta__\""));
        assert!(create.ends_with("ENGINE = Memory"));

        let upsert = ClickHouseDialect.cache_meta_upsert_sql(
            "m",
            "k1",
            "SELECT 'it''s \\ done'",
            "__ggsql_cache_k1__",
            10,
            20,
            30,
        );
        assert_eq!(upsert.len(), 2);
        assert_eq!(upsert[0], "ALTER TABLE \"m\" DELETE WHERE cache_key = 'k1'");
        assert!(upsert[1].starts_with("INSERT INTO \"m\" "));
        assert!(
            upsert[1].contains("'SELECT ''it''''s \\\\ done'''"),
            "quotes doubled and backslashes escaped: {}",
            upsert[1]
        );
        assert!(upsert[1].ends_with(
            "VALUES ('k1', 'SELECT ''it''''s \\\\ done''', '__ggsql_cache_k1__', 10, 10, 20, 30)"
        ));

        assert_eq!(
            ClickHouseDialect.cache_meta_touch_sql("m", "k1", 99),
            "ALTER TABLE \"m\" UPDATE last_accessed_epoch_ms = 99 WHERE cache_key = 'k1'"
        );
        assert_eq!(
            ClickHouseDialect.cache_meta_delete_sql("m", "k1"),
            "ALTER TABLE \"m\" DELETE WHERE cache_key = 'k1'"
        );
    }
}

/// Behavioural tests shared by every transport. Each transport's test module
/// calls these with a connected reader; they use table names unique to the
/// caller so transports sharing one engine (chDB has a single in-process
/// connection) do not collide.
#[cfg(test)]
pub(crate) mod live_tests {
    use super::*;

    pub(crate) fn basic_types<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        let df = reader
            .execute_sql(
                "SELECT toDateTime('2024-01-02 03:04:05', 'UTC') AS dt, \
                        toDate('2024-01-02') AS d, \
                        'x' AS s, toLowCardinality('lc') AS lc, \
                        1 AS u8, toInt64(-5) AS i64, 1.5 AS f, true AS b, \
                        CAST(NULL AS Nullable(Int32)) AS n, \
                        CAST('a' AS Enum8('a' = 1, 'b' = 2)) AS e, \
                        toUUID('61f0c404-5cb3-11e7-907b-a6006ad3dba0') AS uid, \
                        toIPv4('1.2.3.4') AS ip, toDecimal64(2.5, 2) AS dec, \
                        toFixedString('ab', 4) AS fs, toInt128(7) AS big, \
                        [1, 2] AS arr, toDateTime64('2024-01-02 03:04:05.123', 3, 'UTC') AS dt64",
            )
            .unwrap();
        assert_eq!(df.height(), 1);
        // Every timestamp arrives normalized to naive microseconds.
        let us = DataType::Timestamp(TimeUnit::Microsecond, None);
        assert_eq!(df.column_dtype("dt").unwrap(), us);
        assert_eq!(df.column_dtype("dt64").unwrap(), us);
        let dt64 = crate::array_util::as_timestamp_us(df.column("dt64").unwrap()).unwrap();
        assert_eq!(dt64.value(0), 1_704_164_645_123_000);
        assert_eq!(df.column_dtype("d").unwrap(), DataType::Date32);
        assert_eq!(df.column_dtype("s").unwrap(), DataType::Utf8);
        assert_eq!(df.column_dtype("lc").unwrap(), DataType::Utf8);
        assert_eq!(df.column_dtype("u8").unwrap(), DataType::UInt8);
        assert_eq!(df.column_dtype("i64").unwrap(), DataType::Int64);
        assert_eq!(df.column_dtype("f").unwrap(), DataType::Float64);
        assert_eq!(df.column_dtype("b").unwrap(), DataType::Boolean);
        assert_eq!(df.column_dtype("n").unwrap(), DataType::Int32);
        assert!(df.column("n").unwrap().is_null(0));
        assert_eq!(df.column_dtype("e").unwrap(), DataType::Utf8);
        assert_eq!(value_to_string(df.column("e").unwrap(), 0), "a");
        assert_eq!(
            value_to_string(df.column("uid").unwrap(), 0),
            "61f0c404-5cb3-11e7-907b-a6006ad3dba0"
        );
        assert_eq!(value_to_string(df.column("ip").unwrap(), 0), "1.2.3.4");
        assert_eq!(df.column_dtype("dec").unwrap(), DataType::Float64);
        assert_eq!(value_to_string(df.column("fs").unwrap(), 0), "ab");
        assert_eq!(value_to_string(df.column("big").unwrap(), 0), "7");
        assert_eq!(value_to_string(df.column("arr").unwrap(), 0), "[1,2]");

        // The DateTime survived as an instant: 2024-01-02T03:04:05Z.
        let ts = arrow::compute::cast(
            df.column("dt").unwrap(),
            &DataType::Timestamp(TimeUnit::Microsecond, None),
        )
        .unwrap();
        let ts = crate::array_util::as_timestamp_us(&ts).unwrap();
        assert_eq!(ts.value(0), 1_704_164_645_000_000);
    }

    pub(crate) fn empty_result_keeps_schema<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        let df = reader
            .execute_sql("SELECT number AS x, toString(number) AS s FROM numbers(3) WHERE 0")
            .unwrap();
        assert_eq!(df.height(), 0);
        assert_eq!(df.get_column_names(), vec!["x", "s"]);
    }

    pub(crate) fn ddl_and_errors<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        assert_eq!(
            reader.execute_sql("SET max_threads = 2").unwrap().height(),
            0
        );
        let df = reader
            .execute_sql("SELECT getSetting('max_threads') AS v")
            .unwrap();
        assert_eq!(value_to_string(df.column("v").unwrap(), 0), "2");

        let err = reader
            .execute_sql("SELECT * FROM __ggsql_no_such_table__")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("UNKNOWN_TABLE") || err.contains("does not exist"),
            "{err}"
        );

        let err = reader.execute_sql("SELEC 1").unwrap_err().to_string();
        assert!(err.contains("Syntax error"), "{err}");
    }

    pub(crate) fn register_roundtrip<T: Transport>(reader: &ClickHouseSqlReader<T>, table: &str) {
        let q = naming::quote_ident(table);
        let df = crate::df! {
            "x" => vec![1_i64, 2, 3],
            "label" => vec!["a", "b", "c"],
            "v" => vec![1.5_f64, 2.5, 3.5],
        }
        .unwrap();
        reader.register(table, df, true).unwrap();
        let back = reader
            .execute_sql(&format!("SELECT x, label, v FROM {q} ORDER BY x"))
            .unwrap();
        assert_eq!(back.height(), 3);
        assert_eq!(back.column_dtype("x").unwrap(), DataType::Int64);
        assert_eq!(back.column_dtype("label").unwrap(), DataType::Utf8);
        assert_eq!(back.column_dtype("v").unwrap(), DataType::Float64);
        assert_eq!(value_to_string(back.column("label").unwrap(), 2), "c");

        // Without replace the second registration is refused.
        let df2 = crate::df! { "x" => vec![9_i64] }.unwrap();
        assert!(reader.register(table, df2, false).is_err());

        reader.unregister(table).unwrap();
        assert!(reader.execute_sql(&format!("SELECT * FROM {q}")).is_err());
        assert!(reader.unregister(table).is_err());
    }

    pub(crate) fn temp_tables_persist<T: Transport>(reader: &ClickHouseSqlReader<T>, table: &str) {
        let q = naming::quote_ident(table);
        assert!(reader.supports_temporary_tables());
        reader
            .materialize_table(table, &[], "SELECT number AS n FROM numbers(4)")
            .unwrap();
        let df = reader
            .execute_sql(&format!("SELECT count() AS c FROM {q}"))
            .unwrap();
        assert_eq!(value_to_string(df.column("c").unwrap(), 0), "4");
        // Re-materializing replaces rather than failing on "already exists".
        reader
            .materialize_table(table, &["m".to_string()], "SELECT 1")
            .unwrap();
        let df = reader.execute_sql(&format!("SELECT m FROM {q}")).unwrap();
        assert_eq!(df.height(), 1);
        reader
            .execute_sql(&format!("DROP TEMPORARY TABLE {q}"))
            .unwrap();
    }

    pub(crate) fn schema_introspection<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        let catalogs = reader.list_catalogs().unwrap();
        assert!(catalogs.iter().any(|c| c == "default"), "{catalogs:?}");
        assert!(!catalogs.iter().any(|c| c == "system"));
        assert_eq!(reader.list_schemas("default").unwrap(), vec!["default"]);
        let tables = reader.list_tables("system", "system").unwrap();
        assert!(
            tables.iter().any(|t| t.name == "numbers"),
            "system.numbers missing"
        );
        let cols = reader.list_columns("system", "system", "numbers").unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "number");
        assert_eq!(cols[0].data_type, "UInt64");
    }

    #[cfg(feature = "vegalite")]
    pub(crate) fn execute_pipeline<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        use crate::writer::{VegaLiteWriter, Writer};

        // Scatter with a DateTime axis and an Enum colour: both go through the
        // type rewrite and a scale-driven cast. The Enum column lives in a
        // temp table because the ggsql grammar does not parse `Enum8('a' = 1)`
        // type arguments inside a query.
        let table = naming::quote_ident(&format!("__ggsql_enum_{}__", naming::session_id()));
        reader
            .execute_sql(&format!(
                "CREATE TEMPORARY TABLE {table} \
                 (day DateTime('UTC'), y UInt64, parity Enum8('even' = 1, 'odd' = 2))"
            ))
            .unwrap();
        reader
            .execute_sql(&format!(
                "INSERT INTO {table} SELECT toDateTime('2024-01-01 00:00:00', 'UTC') + number * 86400, \
                        number * number, if(number % 2 = 0, 'even', 'odd') FROM numbers(10)"
            ))
            .unwrap();
        let spec = reader
            .execute(&format!(
                "SELECT day, y, parity FROM {table} \
                 VISUALISE day AS x, y AS y, parity AS color DRAW point"
            ))
            .unwrap();
        reader
            .execute_sql(&format!("DROP TEMPORARY TABLE {table}"))
            .unwrap();
        assert_eq!(spec.metadata().rows, 10);
        let json = VegaLiteWriter::new().render(&spec).unwrap();
        assert!(
            json.contains("\"temporal\""),
            "date axis should be temporal"
        );
        assert!(json.contains("even"), "enum labels should survive: {json}");

        // Stat transforms run on the engine: histogram binning and a boxplot
        // (quantiles) over a global temp table.
        let spec = reader
            .execute(
                "SELECT number % 7 AS g, sin(number) * 10 AS v FROM numbers(200) \
                 VISUALISE v AS x DRAW histogram SETTING bins => 10",
            )
            .unwrap();
        assert!(spec.layer_data(0).unwrap().height() > 1);

        let spec = reader
            .execute(
                "SELECT toString(number % 3) AS g, number AS v FROM numbers(30) \
                 VISUALISE g AS x, v AS y DRAW boxplot",
            )
            .unwrap();
        // Three groups, each summarized as box, median and two whiskers.
        assert_eq!(spec.layer_data(0).unwrap().height(), 12);

        // Bar with count stat plus a CTE that is materialized as a temp table.
        let spec = reader
            .execute(
                "WITH t AS (SELECT number % 4 AS k FROM numbers(40)) \
                 SELECT toString(k) AS k FROM t \
                 VISUALISE k AS x DRAW bar",
            )
            .unwrap();
        assert_eq!(spec.layer_data(0).unwrap().height(), 4);
    }

    #[cfg(all(feature = "builtin-data", feature = "parquet"))]
    pub(crate) fn builtin_dataset<T: Transport>(reader: &ClickHouseSqlReader<T>) {
        let df = reader
            .execute_sql("SELECT count() AS c FROM ggsql:penguins")
            .unwrap();
        assert_eq!(value_to_string(df.column("c").unwrap(), 0), "344");
        // Second reference reuses the session's temp table.
        let df = reader
            .execute_sql(
                "SELECT species, count() AS c FROM ggsql:penguins GROUP BY species ORDER BY species",
            )
            .unwrap();
        assert_eq!(df.height(), 3);
    }
}
