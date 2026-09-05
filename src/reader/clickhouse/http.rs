//! ClickHouse server access over the HTTP interface.
//!
//! Plain HTTP(S) requests: the SQL travels in the request body, results come
//! back as `FORMAT ArrowStream`, and registered DataFrames are bulk-loaded with
//! `INSERT … FORMAT ArrowStream`. No client library or native-protocol driver
//! is needed.
//!
//! # Connection strings
//!
//! ```text
//! clickhouse://[user[:password]@]host[:port][/database][?param=value&…]
//! clickhouses://…                                 TLS; default port 8443
//! ```
//!
//! Every query parameter other than `secure`, `user`, `password` and
//! `database` is forwarded verbatim to the server on each request, so any
//! ClickHouse setting or HTTP-interface parameter can be set per connection
//! (`?session_timezone=UTC&max_threads=4&session_timeout=3600`).
//!
//! When the URI omits them, the host, user and password fall back to the
//! `CLICKHOUSE_HOST`, `CLICKHOUSE_USER` and `CLICKHOUSE_PASSWORD` environment
//! variables, so credentials need not appear in the connection string.
//!
//! Each transport owns one server session (a fresh `session_id`), so temporary
//! tables and `SET` statements persist across requests.

use std::time::Duration;

use super::{percent_decode, ClickHouseSqlReader, Transport};
use crate::{GgsqlError, Result};

const DEFAULT_HTTP_PORT: u16 = 8123;
const DEFAULT_HTTPS_PORT: u16 = 8443;

/// Reader for a ClickHouse server reached over HTTP.
pub type ClickHouseReader = ClickHouseSqlReader<HttpTransport>;

impl ClickHouseReader {
    /// Create a reader from a `clickhouse://` / `clickhouses://` connection
    /// string. Connects eagerly.
    pub fn from_connection_string(uri: &str) -> Result<Self> {
        Self::new(ClickHouseConfig::from_uri(uri)?)
    }

    /// Create a reader from an already-parsed configuration. Connects eagerly.
    pub fn new(config: ClickHouseConfig) -> Result<Self> {
        Self::from_transport(HttpTransport::new(config))
    }

    /// The parsed connection configuration.
    pub fn config(&self) -> &ClickHouseConfig {
        &self.transport().config
    }
}

// =============================================================================
// Connection configuration
// =============================================================================

/// Parsed form of a `clickhouse://` / `clickhouses://` connection string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickHouseConfig {
    /// Server endpoint including scheme and port, e.g. `http://localhost:8123`.
    pub base_url: String,
    /// Default database for unqualified table names (`None` = server default).
    pub database: Option<String>,
    pub user: String,
    pub password: String,
    /// Extra URL parameters forwarded to the server on every request.
    pub params: Vec<(String, String)>,
}

impl ClickHouseConfig {
    /// Parse a connection string. See the [module docs](self) for the format.
    pub fn from_uri(uri: &str) -> Result<Self> {
        let (mut secure, rest) = if let Some(rest) = uri.strip_prefix("clickhouses://") {
            (true, rest)
        } else if let Some(rest) = uri.strip_prefix("clickhouse://") {
            (false, rest)
        } else {
            return Err(GgsqlError::ReaderError(format!(
                "Invalid ClickHouse connection string '{uri}': expected clickhouse:// or clickhouses://"
            )));
        };

        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };

        let (userinfo, hostpath) = match rest.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, rest),
        };

        let (hostport, database) = match hostpath.split_once('/') {
            Some((h, d)) if !d.is_empty() => (h, Some(percent_decode(d))),
            Some((h, _)) => (h, None),
            None => (hostpath, None),
        };

        let (mut user, mut password) = match userinfo {
            Some(info) => match info.split_once(':') {
                Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
                None => (Some(percent_decode(info)), None),
            },
            None => (None, None),
        };
        let mut database = database;

        let mut params = Vec::new();
        if let Some(query) = query {
            for segment in query.split('&').filter(|s| !s.is_empty()) {
                let (key, value) = segment.split_once('=').unwrap_or((segment, ""));
                let value = percent_decode(value);
                match key {
                    "secure" => {
                        secure = matches!(
                            value.to_ascii_lowercase().as_str(),
                            "" | "1" | "true" | "yes"
                        )
                    }
                    "user" => user = Some(value),
                    "password" => password = Some(value),
                    "database" => database = Some(value),
                    _ => params.push((key.to_string(), value)),
                }
            }
        }

        let (host, port) = split_host_port(hostport)?;
        let host = if host.is_empty() {
            std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_string())
        } else {
            host.to_string()
        };
        let port = port.unwrap_or(if secure {
            DEFAULT_HTTPS_PORT
        } else {
            DEFAULT_HTTP_PORT
        });
        let scheme = if secure { "https" } else { "http" };

        Ok(Self {
            base_url: format!("{scheme}://{host}:{port}"),
            database,
            user: user
                .or_else(|| std::env::var("CLICKHOUSE_USER").ok())
                .unwrap_or_else(|| "default".to_string()),
            password: password
                .or_else(|| std::env::var("CLICKHOUSE_PASSWORD").ok())
                .unwrap_or_default(),
            params,
        })
    }
}

/// Split `host[:port]`, accepting bracketed IPv6 literals (`[::1]:8123`).
fn split_host_port(hostport: &str) -> Result<(&str, Option<u16>)> {
    let parse_port = |p: &str| -> Result<u16> {
        p.parse::<u16>().map_err(|_| {
            GgsqlError::ReaderError(format!(
                "Invalid port '{p}' in ClickHouse connection string"
            ))
        })
    };
    if hostport.starts_with('[') {
        let end = hostport.find(']').ok_or_else(|| {
            GgsqlError::ReaderError(format!(
                "Unterminated IPv6 literal in ClickHouse host '{hostport}'"
            ))
        })?;
        let host = &hostport[..=end];
        return match hostport[end + 1..].strip_prefix(':') {
            Some(p) => Ok((host, Some(parse_port(p)?))),
            None => Ok((host, None)),
        };
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) => Ok((host, Some(parse_port(port)?))),
        None => Ok((hostport, None)),
    }
}

// =============================================================================
// Transport
// =============================================================================

/// One HTTP session against a ClickHouse server.
pub struct HttpTransport {
    agent: ureq::Agent,
    config: ClickHouseConfig,
    session_id: String,
}

/// Body limit for responses. ClickHouse results are read fully into memory.
const RESPONSE_LIMIT: u64 = u64::MAX;

impl HttpTransport {
    pub fn new(config: ClickHouseConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .user_agent(format!("ggsql/{}", crate::VERSION))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(agent_config),
            config,
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Build a request carrying the session, credentials, output format and
    /// per-connection parameters. `query` puts the SQL in the URL so the body
    /// can carry data (used for `INSERT … FORMAT ArrowStream`).
    fn request(
        &self,
        format: &str,
        query: Option<&str>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut req = self
            .agent
            .post(format!("{}/", self.config.base_url))
            .header("X-ClickHouse-User", &self.config.user)
            .header("X-ClickHouse-Key", &self.config.password)
            .query("session_id", &self.session_id)
            .query("default_format", format)
            // Buffer the whole result server-side so an error raised while
            // streaming still surfaces as a non-200 status with a message,
            // instead of garbage appended to a partial Arrow stream.
            .query("wait_end_of_query", "1")
            // Compress responses; ureq decompresses transparently.
            .query("enable_http_compression", "1");
        if let Some(db) = &self.config.database {
            req = req.query("database", db);
        }
        if let Some(q) = query {
            req = req.query("query", q);
        }
        for (k, v) in &self.config.params {
            req = req.query(k, v);
        }
        req
    }

    /// Send a request and return the response body, turning any non-200
    /// status into a `ReaderError` carrying the server's message.
    fn send(
        &self,
        req: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: impl ureq::AsSendBody,
    ) -> Result<Vec<u8>> {
        let mut resp = req.send(body).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "ClickHouse request to {} failed: {e}",
                self.config.base_url
            ))
        })?;
        let status = resp.status().as_u16();
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(RESPONSE_LIMIT)
            .read_to_vec()
            .map_err(|e| {
                GgsqlError::ReaderError(format!("Failed to read ClickHouse response: {e}"))
            })?;
        if status != 200 {
            let message = String::from_utf8_lossy(&bytes).trim().to_string();
            let message = if message.is_empty() {
                format!("HTTP status {status}")
            } else {
                message
            };
            return Err(GgsqlError::ReaderError(format!(
                "ClickHouse error: {message}"
            )));
        }
        Ok(bytes)
    }
}

impl Transport for HttpTransport {
    fn run(&self, sql: &str, format: &str) -> Result<Vec<u8>> {
        self.send(self.request(format, None), sql)
    }

    fn insert_arrow(&self, table: &str, ipc: &[u8]) -> Result<()> {
        let insert = format!("INSERT INTO {table} FORMAT ArrowStream");
        self.send(self.request("TabSeparated", Some(&insert)), ipc)?;
        Ok(())
    }

    fn endpoint(&self) -> String {
        self.config.base_url.clone()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Connection strings (no server needed) -----------------------------

    #[test]
    fn test_config_full_uri() {
        let c = ClickHouseConfig::from_uri(
            "clickhouse://alice:s%40cret@db.example.com:9999/analytics?max_threads=4&session_timezone=UTC",
        )
        .unwrap();
        assert_eq!(c.base_url, "http://db.example.com:9999");
        assert_eq!(c.database.as_deref(), Some("analytics"));
        assert_eq!(c.user, "alice");
        assert_eq!(c.password, "s@cret");
        assert_eq!(
            c.params,
            vec![
                ("max_threads".to_string(), "4".to_string()),
                ("session_timezone".to_string(), "UTC".to_string()),
            ]
        );
    }

    #[test]
    fn test_config_defaults() {
        let c = ClickHouseConfig::from_uri("clickhouse://myhost").unwrap();
        assert_eq!(c.base_url, "http://myhost:8123");
        assert_eq!(c.database, None);
        assert!(c.params.is_empty());

        let c = ClickHouseConfig::from_uri("clickhouse://myhost/").unwrap();
        assert_eq!(c.database, None);
    }

    #[test]
    fn test_config_secure() {
        let c = ClickHouseConfig::from_uri("clickhouses://play.clickhouse.com").unwrap();
        assert_eq!(c.base_url, "https://play.clickhouse.com:8443");

        let c =
            ClickHouseConfig::from_uri("clickhouses://explorer@play.clickhouse.com:443").unwrap();
        assert_eq!(c.base_url, "https://play.clickhouse.com:443");
        assert_eq!(c.user, "explorer");

        let c = ClickHouseConfig::from_uri("clickhouse://h?secure=1").unwrap();
        assert_eq!(c.base_url, "https://h:8443");
        assert!(c.params.is_empty(), "secure is consumed, not forwarded");

        let c = ClickHouseConfig::from_uri("clickhouses://h?secure=false").unwrap();
        assert_eq!(c.base_url, "http://h:8123");
    }

    #[test]
    fn test_config_credentials_as_params() {
        let c =
            ClickHouseConfig::from_uri("clickhouse://h/?user=bob&password=pw&database=db").unwrap();
        assert_eq!(c.user, "bob");
        assert_eq!(c.password, "pw");
        assert_eq!(c.database.as_deref(), Some("db"));
        assert!(c.params.is_empty());
    }

    #[test]
    fn test_config_ipv6() {
        let c = ClickHouseConfig::from_uri("clickhouse://[::1]:8124/db").unwrap();
        assert_eq!(c.base_url, "http://[::1]:8124");
        assert_eq!(c.database.as_deref(), Some("db"));

        let c = ClickHouseConfig::from_uri("clickhouse://[::1]").unwrap();
        assert_eq!(c.base_url, "http://[::1]:8123");
    }

    #[test]
    fn test_config_password_with_at_sign() {
        let c = ClickHouseConfig::from_uri("clickhouse://u:p@ss@h").unwrap();
        assert_eq!(c.user, "u");
        assert_eq!(c.password, "p@ss");
        assert_eq!(c.base_url, "http://h:8123");
    }

    #[test]
    fn test_config_rejects_other_schemes_and_bad_ports() {
        assert!(ClickHouseConfig::from_uri("duckdb://memory").is_err());
        assert!(ClickHouseConfig::from_uri("clickhouse://h:notaport").is_err());
        assert!(ClickHouseConfig::from_uri("clickhouse://[::1").is_err());
    }

    // ---- Against a live server ---------------------------------------------
    //
    // Set GGSQL_CLICKHOUSE_URI (e.g. `clickhouse://localhost:8123`) to run
    // these; they are skipped otherwise.

    use super::super::live_tests as shared;

    fn live_reader() -> Option<ClickHouseReader> {
        let uri = std::env::var("GGSQL_CLICKHOUSE_URI").ok()?;
        Some(
            ClickHouseReader::from_connection_string(&uri)
                .unwrap_or_else(|e| panic!("cannot connect to {uri}: {e}")),
        )
    }

    #[test]
    fn live_basic_types() {
        let Some(reader) = live_reader() else { return };
        shared::basic_types(&reader);
    }

    #[test]
    fn live_empty_result_keeps_schema() {
        let Some(reader) = live_reader() else { return };
        shared::empty_result_keeps_schema(&reader);
    }

    #[test]
    fn live_ddl_and_errors() {
        let Some(reader) = live_reader() else { return };
        shared::ddl_and_errors(&reader);
    }

    #[test]
    fn live_register_roundtrip() {
        let Some(reader) = live_reader() else { return };
        shared::register_roundtrip(&reader, "__ggsql_http_reg__");
    }

    #[test]
    fn live_temp_tables_persist_within_session() {
        let Some(reader) = live_reader() else { return };
        shared::temp_tables_persist(&reader, "__ggsql_http_mat__");
    }

    #[test]
    fn live_schema_introspection() {
        let Some(reader) = live_reader() else { return };
        shared::schema_introspection(&reader);
    }

    #[cfg(feature = "vegalite")]
    #[test]
    fn live_execute_pipeline() {
        let Some(reader) = live_reader() else { return };
        shared::execute_pipeline(&reader);
    }

    #[cfg(all(feature = "builtin-data", feature = "parquet"))]
    #[test]
    fn live_builtin_dataset() {
        let Some(reader) = live_reader() else { return };
        shared::builtin_dataset(&reader);
    }
}
