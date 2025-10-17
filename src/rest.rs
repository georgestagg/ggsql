/*!
VizQL REST API Server

Provides HTTP endpoints for executing VizQL queries and returning visualization outputs.

## Usage

```bash
vizql-rest --host 127.0.0.1 --port 3000
```

## Endpoints

- `POST /api/v1/query` - Execute a VizQL query
- `POST /api/v1/parse` - Parse a VizQL query (debugging)
- `GET /api/v1/health` - Health check
- `GET /api/v1/version` - Version information
*/

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use vizql::{parser, VizqlError, VERSION};

#[cfg(feature = "duckdb")]
use vizql::reader::{DuckDBReader, Reader};

#[cfg(feature = "vegalite")]
use vizql::writer::{VegaLiteWriter, Writer};

/// CLI arguments for the REST API server
#[derive(Parser)]
#[command(name = "vizql-rest")]
#[command(about = "VizQL REST API Server")]
#[command(version = VERSION)]
struct Cli {
    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port number to bind to
    #[arg(long, default_value = "3000")]
    port: u16,

    /// CORS allowed origins (comma-separated)
    #[arg(long, default_value = "*")]
    cors_origin: String,
}

/// Shared application state
#[derive(Clone)]
struct AppState {
    // Future: Add connection pools here
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request body for /api/v1/query endpoint
#[derive(Debug, Deserialize)]
struct QueryRequest {
    /// VizQL query to execute
    query: String,
    /// Data source connection string (optional, default: duckdb://memory)
    #[serde(default = "default_reader")]
    reader: String,
    /// Output writer format (optional, default: vegalite)
    #[serde(default = "default_writer")]
    writer: String,
}

fn default_reader() -> String {
    "duckdb://memory".to_string()
}

fn default_writer() -> String {
    "vegalite".to_string()
}

/// Request body for /api/v1/parse endpoint
#[derive(Debug, Deserialize)]
struct ParseRequest {
    /// VizQL query to parse
    query: String,
}

/// Successful API response
#[derive(Debug, Serialize)]
struct ApiSuccess<T> {
    status: String,
    data: T,
}

/// Error API response
#[derive(Debug, Serialize)]
struct ApiError {
    status: String,
    error: ErrorDetails,
}

#[derive(Debug, Serialize)]
struct ErrorDetails {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
}

/// Query execution result data
#[derive(Debug, Serialize)]
struct QueryResult {
    /// The visualization specification (Vega-Lite JSON, etc.)
    spec: serde_json::Value,
    /// Metadata about the query execution
    metadata: QueryMetadata,
}

#[derive(Debug, Serialize)]
struct QueryMetadata {
    rows: usize,
    columns: Vec<String>,
    viz_type: String,
    layers: usize,
}

/// Parse result data
#[derive(Debug, Serialize)]
struct ParseResult {
    sql_portion: String,
    viz_portion: String,
    specs: Vec<serde_json::Value>,
}

/// Health check response
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

/// Version response
#[derive(Debug, Serialize)]
struct VersionResponse {
    version: String,
    features: Vec<String>,
}

// ============================================================================
// Error Handling
// ============================================================================

/// Custom error type for API responses
struct ApiErrorResponse {
    status: StatusCode,
    error: ApiError,
}

impl IntoResponse for ApiErrorResponse {
    fn into_response(self) -> Response {
        let json = Json(self.error);
        (self.status, json).into_response()
    }
}

impl From<VizqlError> for ApiErrorResponse {
    fn from(err: VizqlError) -> Self {
        let (status, error_type) = match &err {
            VizqlError::ParseError(_) => (StatusCode::BAD_REQUEST, "ParseError"),
            VizqlError::ValidationError(_) => (StatusCode::BAD_REQUEST, "ValidationError"),
            VizqlError::ReaderError(_) => (StatusCode::BAD_REQUEST, "ReaderError"),
            VizqlError::WriterError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "WriterError"),
            VizqlError::InternalError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "InternalError"),
        };

        ApiErrorResponse {
            status,
            error: ApiError {
                status: "error".to_string(),
                error: ErrorDetails {
                    message: err.to_string(),
                    error_type: error_type.to_string(),
                },
            },
        }
    }
}

impl From<String> for ApiErrorResponse {
    fn from(msg: String) -> Self {
        ApiErrorResponse {
            status: StatusCode::BAD_REQUEST,
            error: ApiError {
                status: "error".to_string(),
                error: ErrorDetails {
                    message: msg,
                    error_type: "BadRequest".to_string(),
                },
            },
        }
    }
}

// ============================================================================
// Handler Functions
// ============================================================================

/// POST /api/v1/query - Execute a VizQL query
async fn query_handler(
    State(_state): State<AppState>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<ApiSuccess<QueryResult>>, ApiErrorResponse> {
    info!("Executing query: {} chars", request.query.len());
    info!("Reader: {}, Writer: {}", request.reader, request.writer);

    // Split query into SQL and VizQL portions
    let (sql_part, _viz_part) = parser::split_query(&request.query)?;

    // Execute SQL portion using the reader
    #[cfg(feature = "duckdb")]
    if request.reader.starts_with("duckdb://") {
        let reader = DuckDBReader::from_connection_string(&request.reader)?;
        let df = reader.execute(&sql_part)?;

        // Parse VizQL portion
        let specs = parser::parse_query(&request.query)?;

        if specs.is_empty() {
            return Err(ApiErrorResponse::from(
                "No visualization specifications found".to_string(),
            ));
        }

        // Get metadata
        let (rows, _cols) = df.shape();
        let columns: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
        let first_spec = &specs[0];

        // Generate visualization output using writer
        #[cfg(feature = "vegalite")]
        if request.writer == "vegalite" {
            let writer = VegaLiteWriter::new();
            let json_output = writer.write(first_spec, &df)?;
            let spec_value: serde_json::Value = serde_json::from_str(&json_output)
                .map_err(|e| VizqlError::WriterError(format!("Failed to parse JSON: {}", e)))?;

            let result = QueryResult {
                spec: spec_value,
                metadata: QueryMetadata {
                    rows,
                    columns,
                    viz_type: format!("{:?}", first_spec.viz_type),
                    layers: first_spec.layers.len(),
                },
            };

            return Ok(Json(ApiSuccess {
                status: "success".to_string(),
                data: result,
            }));
        }

        #[cfg(not(feature = "vegalite"))]
        return Err(ApiErrorResponse::from(
            "VegaLite writer not available".to_string(),
        ));
    }

    #[cfg(not(feature = "duckdb"))]
    return Err(ApiErrorResponse::from(
        "DuckDB reader not available".to_string(),
    ));

    #[cfg(feature = "duckdb")]
    Err(ApiErrorResponse::from(format!(
        "Unsupported reader: {}",
        request.reader
    )))
}

/// POST /api/v1/parse - Parse a VizQL query
async fn parse_handler(
    Json(request): Json<ParseRequest>,
) -> Result<Json<ApiSuccess<ParseResult>>, ApiErrorResponse> {
    info!("Parsing query: {} chars", request.query.len());

    // Split query
    let (sql_part, viz_part) = parser::split_query(&request.query)?;

    // Parse VizQL portion
    let specs = parser::parse_query(&request.query)?;

    // Convert specs to JSON
    let specs_json: Vec<serde_json::Value> = specs
        .iter()
        .map(|spec| serde_json::to_value(spec).unwrap_or(serde_json::Value::Null))
        .collect();

    let result = ParseResult {
        sql_portion: sql_part,
        viz_portion: viz_part,
        specs: specs_json,
    };

    Ok(Json(ApiSuccess {
        status: "success".to_string(),
        data: result,
    }))
}

/// GET /api/v1/health - Health check
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: VERSION.to_string(),
    })
}

/// GET /api/v1/version - Version information
async fn version_handler() -> Json<VersionResponse> {
    let mut features = Vec::new();

    #[cfg(feature = "duckdb")]
    features.push("duckdb".to_string());

    #[cfg(feature = "vegalite")]
    features.push("vegalite".to_string());

    #[cfg(feature = "sqlite")]
    features.push("sqlite".to_string());

    #[cfg(feature = "postgres")]
    features.push("postgres".to_string());

    Json(VersionResponse {
        version: VERSION.to_string(),
        features,
    })
}

/// Root handler
async fn root_handler() -> &'static str {
    "VizQL REST API Server - See /api/v1/health for status"
}

// ============================================================================
// Main Server
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vizql_rest=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Parse CLI arguments
    let cli = Cli::parse();

    // Create application state
    let state = AppState {};

    // Configure CORS
    let cors = if cli.cors_origin == "*" {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(vec![header::CONTENT_TYPE])
    } else {
        let origins: Vec<_> = cli
            .cors_origin
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(vec![header::CONTENT_TYPE])
    };

    // Build router
    let app = Router::new()
        .route("/", get(root_handler))
        .route("/api/v1/query", post(query_handler))
        .route("/api/v1/parse", post(parse_handler))
        .route("/api/v1/health", get(health_handler))
        .route("/api/v1/version", get(version_handler))
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    // Parse bind address
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .expect("Invalid host or port");

    info!("Starting VizQL REST API server on {}", addr);
    info!("API documentation:");
    info!("  POST /api/v1/query  - Execute VizQL query");
    info!("  POST /api/v1/parse  - Parse VizQL query");
    info!("  GET  /api/v1/health - Health check");
    info!("  GET  /api/v1/version - Version info");

    // Start server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
