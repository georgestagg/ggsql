//! Embedded ClickHouse via [chDB](https://clickhouse.com/chdb) (libchdb).
//!
//! libchdb is loaded at runtime with `libloading`, so the `chdb` feature adds
//! no build-time dependency: a build with the feature enabled works everywhere,
//! and only a `chdb://` connection (or a `chdb+…://` cache) needs the library
//! present. It is searched in `GGSQL_CHDB_LIBRARY`, then the system library
//! path and the usual install locations. Install it with
//! `curl -sL https://lib.chdb.io | bash`.
//!
//! # Connection strings
//!
//! ```text
//! chdb://                 in-memory engine (also chdb://memory, chdb://:memory:)
//! chdb:///path/to/dir     persistent state under a directory
//! chdb://…?key=value      extra `--key=value` engine arguments
//! ```
//!
//! libchdb keeps a single connection per process. Readers opened on the same
//! path share it (the connection closes when the last reader drops); opening a
//! second path while one is in use is an error.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::{ch_literal, percent_decode, ClickHouseSqlReader, Transport};
use crate::reader::CacheBackend;
use crate::{GgsqlError, Result};

/// Reader for the embedded chDB engine.
pub type ChdbReader = ClickHouseSqlReader<ChdbTransport>;

/// Path spelling that selects the in-memory engine.
const MEMORY: &str = ":memory:";

impl ChdbReader {
    /// Create a reader from a `chdb://` connection string.
    pub fn from_connection_string(uri: &str) -> Result<Self> {
        let rest = uri.strip_prefix("chdb://").ok_or_else(|| {
            GgsqlError::ReaderError(format!(
                "Invalid chDB connection string '{uri}': expected chdb://"
            ))
        })?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let path = match path {
            "" | "memory" | MEMORY => MEMORY.to_string(),
            p => percent_decode(p),
        };
        let args: Vec<String> = query
            .map(|q| {
                q.split('&')
                    .filter(|s| !s.is_empty())
                    .map(|s| match s.split_once('=') {
                        Some((k, v)) => format!("--{k}={}", percent_decode(v)),
                        None => format!("--{s}"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::from_transport(ChdbTransport::open(&path, &args)?)
    }

    /// An in-memory engine.
    pub fn in_memory() -> Result<Self> {
        Self::from_transport(ChdbTransport::open(MEMORY, &[])?)
    }

    /// An engine whose state persists under `path`.
    pub fn with_path(path: &str) -> Result<Self> {
        Self::from_transport(ChdbTransport::open(path, &[])?)
    }

    /// Version of the loaded libchdb, e.g. `26.7.0`.
    pub fn library_version() -> Result<String> {
        let api = Api::get()?;
        match api.version {
            Some(f) => Ok(unsafe { CStr::from_ptr(f()) }
                .to_string_lossy()
                .into_owned()),
            None => Ok("unknown".to_string()),
        }
    }
}

impl CacheBackend for ChdbReader {
    fn new_in_memory() -> Result<Self> {
        Self::in_memory()
    }
}

// =============================================================================
// FFI
// =============================================================================

// `chdb_connection` is `struct chdb_connection_ *`; `chdb_connect` returns a
// pointer to one (`chdb_connection *`), which is also what `chdb_close_conn`
// takes. Queries take the dereferenced `chdb_connection`.
type Conn = *mut c_void;
type ConnHandle = *mut Conn;
type QueryResult = *mut c_void;

struct Api {
    _lib: libloading::Library,
    connect: unsafe extern "C" fn(c_int, *mut *mut c_char) -> ConnHandle,
    close_conn: unsafe extern "C" fn(ConnHandle),
    query: unsafe extern "C" fn(Conn, *const c_char, *const c_char) -> QueryResult,
    destroy_result: unsafe extern "C" fn(QueryResult),
    result_buffer: unsafe extern "C" fn(QueryResult) -> *mut c_char,
    result_length: unsafe extern "C" fn(QueryResult) -> usize,
    result_error: unsafe extern "C" fn(QueryResult) -> *const c_char,
    version: Option<unsafe extern "C" fn() -> *const c_char>,
}

static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();

impl Api {
    fn get() -> Result<&'static Api> {
        API.get_or_init(Self::load).as_ref().map_err(|e| {
            GgsqlError::ReaderError(format!(
                "chDB is not available: {e}. Install libchdb with `curl -sL https://lib.chdb.io | bash`, \
                 or set GGSQL_CHDB_LIBRARY to the path of libchdb.so"
            ))
        })
    }

    fn candidates() -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(p) = std::env::var("GGSQL_CHDB_LIBRARY") {
            paths.push(PathBuf::from(p));
        }
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        #[cfg(target_os = "macos")]
        {
            paths.push("libchdb.dylib".into());
            paths.push("/usr/local/lib/libchdb.dylib".into());
            paths.push("/opt/homebrew/lib/libchdb.dylib".into());
            if let Some(h) = &home {
                paths.push(h.join(".local/lib/libchdb.dylib"));
            }
        }
        #[cfg(target_os = "windows")]
        {
            paths.push("chdb.dll".into());
            paths.push("libchdb.dll".into());
            let _ = &home;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            paths.push("libchdb.so".into());
            paths.push("/usr/local/lib/libchdb.so".into());
            paths.push("/usr/lib/libchdb.so".into());
            if let Some(h) = &home {
                paths.push(h.join(".local/lib/libchdb.so"));
            }
        }
        paths
    }

    fn load() -> std::result::Result<Api, String> {
        let mut errors = Vec::new();
        for path in Self::candidates() {
            match unsafe { libloading::Library::new(&path) } {
                Ok(lib) => return unsafe { Self::from_library(lib) },
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        Err(format!("libchdb not found ({})", errors.join("; ")))
    }

    unsafe fn from_library(lib: libloading::Library) -> std::result::Result<Api, String> {
        macro_rules! sym {
            ($name:literal) => {
                *lib.get(concat!($name, "\0").as_bytes())
                    .map_err(|e| format!("libchdb lacks {}: {e}", $name))?
            };
        }
        let api = Api {
            connect: sym!("chdb_connect"),
            close_conn: sym!("chdb_close_conn"),
            query: sym!("chdb_query"),
            destroy_result: sym!("chdb_destroy_query_result"),
            result_buffer: sym!("chdb_result_buffer"),
            result_length: sym!("chdb_result_length"),
            result_error: sym!("chdb_result_error"),
            version: lib
                .get::<unsafe extern "C" fn() -> *const c_char>(b"chdb_version\0")
                .ok()
                .map(|s| *s),
            _lib: lib,
        };
        // chDB installs process-wide signal handlers by default, which would
        // hijack the CLI's and the Jupyter kernel's own handling.
        if let Ok(f) = api
            ._lib
            .get::<unsafe extern "C" fn(c_int)>(b"chdb_set_signal_handlers_enabled\0")
        {
            f(0);
        }
        Ok(api)
    }
}

/// The process-wide libchdb connection.
struct Connection {
    api: &'static Api,
    handle: ConnHandle,
    conn: Conn,
    path: String,
    /// Serializes statements: the engine is one session.
    lock: Mutex<()>,
}

// The raw pointers are only ever used through the API, which documents its
// functions as thread-safe, and statements are additionally serialized.
unsafe impl Send for Connection {}
unsafe impl Sync for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { (self.api.close_conn)(self.handle) }
    }
}

static SHARED: Mutex<Weak<Connection>> = Mutex::new(Weak::new());

impl Connection {
    fn open(path: &str, args: &[String]) -> Result<Arc<Connection>> {
        let mut shared = SHARED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = shared.upgrade() {
            if existing.path == path {
                return Ok(existing);
            }
            return Err(GgsqlError::ReaderError(format!(
                "chDB supports one connection per process, and one is already open at '{}'; \
                 cannot also open '{path}'",
                existing.path
            )));
        }

        let api = Api::get()?;
        let mut argv: Vec<CString> = vec![CString::new("clickhouse").unwrap()];
        if path != MEMORY {
            argv.push(CString::new(format!("--path={path}")).map_err(bad_arg)?);
        }
        for a in args {
            argv.push(CString::new(a.as_str()).map_err(bad_arg)?);
        }
        let mut argv_ptrs: Vec<*mut c_char> =
            argv.iter().map(|a| a.as_ptr() as *mut c_char).collect();

        let handle = unsafe { (api.connect)(argv_ptrs.len() as c_int, argv_ptrs.as_mut_ptr()) };
        if handle.is_null() {
            return Err(GgsqlError::ReaderError(format!(
                "chdb_connect failed for path '{path}'"
            )));
        }
        let conn = unsafe { *handle };
        if conn.is_null() {
            return Err(GgsqlError::ReaderError(format!(
                "chdb_connect returned no connection for path '{path}'"
            )));
        }

        let connection = Arc::new(Connection {
            api,
            handle,
            conn,
            path: path.to_string(),
            lock: Mutex::new(()),
        });
        // Results never leave the process, so compressing the Arrow stream
        // only costs CPU.
        connection.query(
            "SET output_format_arrow_compression_method = 'none'",
            "TabSeparated",
        )?;
        *shared = Arc::downgrade(&connection);
        Ok(connection)
    }

    fn query(&self, sql: &str, format: &str) -> Result<Vec<u8>> {
        let csql = CString::new(sql).map_err(bad_arg)?;
        let cfmt = CString::new(format).map_err(bad_arg)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let result = unsafe { (self.api.query)(self.conn, csql.as_ptr(), cfmt.as_ptr()) };
        if result.is_null() {
            return Err(GgsqlError::ReaderError(
                "chDB returned no result (connection closed?)".into(),
            ));
        }
        let outcome = unsafe {
            let err = (self.api.result_error)(result);
            if !err.is_null() {
                let message = CStr::from_ptr(err).to_string_lossy().trim().to_string();
                Err(GgsqlError::ReaderError(format!(
                    "ClickHouse error: {message}"
                )))
            } else {
                let buf = (self.api.result_buffer)(result);
                let len = (self.api.result_length)(result);
                if buf.is_null() || len == 0 {
                    Ok(Vec::new())
                } else {
                    Ok(std::slice::from_raw_parts(buf as *const u8, len).to_vec())
                }
            }
        };
        unsafe { (self.api.destroy_result)(result) };
        outcome
    }
}

fn bad_arg(e: std::ffi::NulError) -> GgsqlError {
    GgsqlError::ReaderError(format!("argument contains a NUL byte: {e}"))
}

// =============================================================================
// Transport
// =============================================================================

/// Statements run in-process on libchdb.
pub struct ChdbTransport {
    connection: Arc<Connection>,
}

impl ChdbTransport {
    /// Open (or share) the process-wide connection for `path`
    /// (`":memory:"` for the in-memory engine), passing `args` as extra
    /// `--key=value` engine arguments on first open.
    pub fn open(path: &str, args: &[String]) -> Result<Self> {
        Ok(Self {
            connection: Connection::open(path, args)?,
        })
    }

    /// The engine path this transport is attached to.
    pub fn path(&self) -> &str {
        &self.connection.path
    }
}

impl Transport for ChdbTransport {
    fn run(&self, sql: &str, format: &str) -> Result<Vec<u8>> {
        self.connection.query(sql, format)
    }

    /// The C API has no input-data channel for `INSERT … FORMAT`, so the
    /// stream goes through a temporary file read back with `FROM INFILE`.
    fn insert_arrow(&self, table: &str, ipc: &[u8]) -> Result<()> {
        let path = std::env::temp_dir().join(format!("ggsql-chdb-{}.arrows", uuid::Uuid::new_v4()));
        std::fs::write(&path, ipc).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to write staging file {}: {e}",
                path.display()
            ))
        })?;
        let sql = format!(
            "INSERT INTO {table} FROM INFILE {} FORMAT ArrowStream",
            ch_literal(&path.to_string_lossy())
        );
        let outcome = self.connection.query(&sql, "TabSeparated");
        let _ = std::fs::remove_file(&path);
        outcome.map(|_| ())
    }

    fn assumes_temporary_tables(&self) -> bool {
        true
    }

    fn endpoint(&self) -> String {
        format!("chdb ({})", self.connection.path)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::super::live_tests as shared;
    use super::*;
    use crate::reader::Reader;

    /// libchdb is one connection per process, and the executor's temp-table
    /// names are per process too, so tests that run ggsql queries on chDB
    /// must not overlap. Each test holds this for its whole body.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// The in-memory engine plus the serialization guard, or `None` (skipping
    /// the test) when libchdb is not installed on this machine.
    fn reader_or_skip() -> Option<(std::sync::MutexGuard<'static, ()>, ChdbReader)> {
        if let Err(e) = Api::get() {
            eprintln!("skipping chDB test: {e}");
            return None;
        }
        let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        Some((
            guard,
            ChdbReader::in_memory().expect("chDB in-memory engine"),
        ))
    }

    #[test]
    fn test_uri_forms_share_one_connection() {
        let Some((_guard, a)) = reader_or_skip() else {
            return;
        };
        // Every in-memory spelling maps to the same process-wide connection.
        let b = ChdbReader::from_connection_string("chdb://").unwrap();
        let c = ChdbReader::from_connection_string("chdb://memory").unwrap();
        let d = ChdbReader::from_connection_string("chdb://:memory:").unwrap();
        for r in [&b, &c, &d] {
            assert_eq!(r.transport().path(), MEMORY);
        }
        assert!(Arc::ptr_eq(
            &a.transport().connection,
            &d.transport().connection
        ));
        // A different path cannot be opened while the shared one is alive.
        let err = ChdbReader::with_path("/tmp/ggsql-chdb-other")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("one connection per process"), "{err}");
        assert!(ChdbReader::from_connection_string("duckdb://memory").is_err());
        assert!(ChdbReader::library_version().unwrap().contains('.'));
    }

    #[test]
    fn live_basic_types() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::basic_types(&reader);
    }

    #[test]
    fn live_empty_result_keeps_schema() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::empty_result_keeps_schema(&reader);
    }

    #[test]
    fn live_ddl_and_errors() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::ddl_and_errors(&reader);
    }

    #[test]
    fn live_register_roundtrip() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::register_roundtrip(&reader, "__ggsql_chdb_reg__");
    }

    #[test]
    fn live_temp_tables_persist() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::temp_tables_persist(&reader, "__ggsql_chdb_mat__");
    }

    #[test]
    fn live_schema_introspection() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::schema_introspection(&reader);
    }

    #[cfg(feature = "vegalite")]
    #[test]
    fn live_execute_pipeline() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::execute_pipeline(&reader);
    }

    #[cfg(all(feature = "builtin-data", feature = "parquet"))]
    #[test]
    fn live_builtin_dataset() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        shared::builtin_dataset(&reader);
    }

    #[test]
    fn live_file_source() {
        let Some((_guard, reader)) = reader_or_skip() else {
            return;
        };
        let path = std::env::temp_dir().join(format!("ggsql-chdb-{}.csv", uuid::Uuid::new_v4()));
        std::fs::write(&path, "a,b\n1,2\n3,4\n").unwrap();
        // The executor spells file sources as `FROM '<path>'`.
        let df = reader
            .execute_sql(&format!("SELECT * FROM '{}' ORDER BY a", path.display()))
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(df.height(), 2);
        assert_eq!(df.get_column_names(), vec!["a", "b"]);
    }

    /// chDB as the cache behind a read-only primary: the executor's temporary
    /// tables, the memo table and its bookkeeping all live on the engine.
    #[test]
    fn live_as_cache_backend() {
        use crate::reader::cache::CachingReader;
        use crate::reader::test_support::ReadOnlyReader;

        let Some((_guard, cache)) = reader_or_skip() else {
            return;
        };
        let primary = ReadOnlyReader::new(Box::new(ChdbReader::in_memory().unwrap()));
        let reader = CachingReader::new(
            Box::new(primary),
            Box::new(cache),
            "test://readonly-primary",
            "chdb",
        );
        let query = "SELECT number AS x, number * 2 AS y FROM numbers(6) \
                     VISUALISE x, y DRAW point";
        let spec = reader.execute(query).unwrap();
        assert_eq!(spec.metadata().rows, 6);
        // Second run is served from the memo; the memo table is queryable.
        let spec = reader.execute(query).unwrap();
        assert_eq!(spec.metadata().rows, 6);
        let meta = reader
            .execute_sql("SELECT cache_key, row_count FROM __ggsql_cache_meta__")
            .unwrap();
        assert!(meta.height() >= 1, "memo rows expected");
        reader.clear_cache().unwrap();
        let meta = reader
            .execute_sql("SELECT cache_key FROM __ggsql_cache_meta__")
            .unwrap();
        assert_eq!(meta.height(), 0);
    }
}
