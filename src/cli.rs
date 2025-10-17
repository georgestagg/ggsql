/*!
VizQL Command Line Interface

Provides commands for executing VizQL queries with various data sources and output formats.
*/

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vizql::{parser, VERSION};

#[cfg(feature = "duckdb")]
use vizql::reader::{Reader, DuckDBReader};

#[cfg(feature = "vegalite")]
use vizql::writer::{Writer, VegaLiteWriter};

#[derive(Parser)]
#[command(name = "vizql")]
#[command(about = "SQL extension for declarative data visualization")]
#[command(version = VERSION)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Execute a VizQL query
    Exec {
        /// The VizQL query to execute
        query: String,

        /// Data source connection string
        #[arg(long, default_value = "duckdb://memory")]
        reader: String,

        /// Output format
        #[arg(long, default_value = "vegalite")]
        writer: String,

        /// Output file path
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Execute a VizQL query from a file
    Run {
        /// Path to .sql file containing VizQL query
        file: PathBuf,

        /// Data source connection string
        #[arg(long, default_value = "duckdb://memory")]
        reader: String,

        /// Output format
        #[arg(long, default_value = "vegalite")]
        writer: String,

        /// Output file path
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Parse a query and show the AST (for debugging)
    Parse {
        /// The VizQL query to parse
        query: String,

        /// Output format for AST (json, debug, pretty)
        #[arg(long, default_value = "pretty")]
        format: String,
    },

    /// Validate a query without executing
    Validate {
        /// The VizQL query to validate
        query: String,

        /// Data source connection string (needed for column validation)
        #[arg(long)]
        reader: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Exec { query, reader, writer, output } => {
            println!("Executing query: {}", query);
            println!("Reader: {}", reader);
            println!("Writer: {}", writer);
            if let Some(ref output_file) = output {
                println!("Output: {}", output_file.display());
            }

            // Split query into SQL and VizQL portions
            match parser::split_query(&query) {
                Ok((sql_part, viz_part)) => {
                    println!("\nQuery split:");
                    println!("  SQL portion: {} chars", sql_part.len());
                    println!("  VizQL portion: {} chars", viz_part.len());

                    // Execute SQL portion using the reader
                    #[cfg(feature = "duckdb")]
                    if reader.starts_with("duckdb://") {
                        match DuckDBReader::from_connection_string(&reader) {
                            Ok(db_reader) => {
                                match db_reader.execute(&sql_part) {
                                    Ok(df) => {
                                        println!("\nQuery executed successfully!");
                                        println!("Result shape: {:?}", df.shape());
                                        println!("Columns: {:?}", df.get_column_names());

                                        // Parse VizQL portion
                                        match parser::parse_query(&query) {
                                            Ok(specs) => {
                                                println!("\nParsed {} visualization spec(s)", specs.len());

                                                // Generate visualization output using writer
                                                #[cfg(feature = "vegalite")]
                                                if writer == "vegalite" {
                                                    let vl_writer = VegaLiteWriter::new();

                                                    // For now, render the first spec only
                                                    if let Some(spec) = specs.first() {
                                                        match vl_writer.write(spec, &df) {
                                                            Ok(json_output) => {
                                                                if let Some(output_path) = &output {
                                                                    // Write to file
                                                                    match std::fs::write(output_path, &json_output) {
                                                                        Ok(_) => println!("\nVega-Lite JSON written to: {}", output_path.display()),
                                                                        Err(e) => {
                                                                            eprintln!("Failed to write output file: {}", e);
                                                                            std::process::exit(1);
                                                                        }
                                                                    }
                                                                } else {
                                                                    // Print to stdout
                                                                    println!("\n{}", json_output);
                                                                }
                                                            }
                                                            Err(e) => {
                                                                eprintln!("Failed to generate Vega-Lite output: {}", e);
                                                                std::process::exit(1);
                                                            }
                                                        }
                                                    } else {
                                                        eprintln!("No visualization specifications found");
                                                        std::process::exit(1);
                                                    }
                                                }

                                                #[cfg(not(feature = "vegalite"))]
                                                {
                                                    if writer == "vegalite" {
                                                        eprintln!("VegaLite writer not compiled in. Rebuild with --features vegalite");
                                                        std::process::exit(1);
                                                    }
                                                }

                                                if writer != "vegalite" {
                                                    println!("\nNote: Writer '{}' not yet implemented", writer);
                                                    println!("Available writers: vegalite");
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Failed to parse VizQL portion: {}", e);
                                                std::process::exit(1);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to execute SQL query: {}", e);
                                        std::process::exit(1);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to create DuckDB reader: {}", e);
                                std::process::exit(1);
                            }
                        }
                    } else {
                        eprintln!("Unsupported reader: {}", reader);
                        eprintln!("Currently only 'duckdb://' readers are supported");
                        std::process::exit(1);
                    }

                    #[cfg(not(feature = "duckdb"))]
                    {
                        eprintln!("No reader support compiled in. Rebuild with --features duckdb");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to split query: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Commands::Run { file, reader, writer, output } => {
            println!("Running query from file: {}", file.display());
            println!("Reader: {}", reader);
            println!("Writer: {}", writer);
            if let Some(ref output_file) = output {
                println!("Output: {}", output_file.display());
            }

            // Read query from file
            match std::fs::read_to_string(&file) {
                Ok(query) => {
                    // Execute the query (reuse exec logic)
                    match parser::split_query(&query) {
                        Ok((sql_part, viz_part)) => {
                            println!("\nQuery split:");
                            println!("  SQL portion: {} chars", sql_part.len());
                            println!("  VizQL portion: {} chars", viz_part.len());

                            // Execute SQL portion using the reader
                            #[cfg(feature = "duckdb")]
                            if reader.starts_with("duckdb://") {
                                match DuckDBReader::from_connection_string(&reader) {
                                    Ok(db_reader) => {
                                        match db_reader.execute(&sql_part) {
                                            Ok(df) => {
                                                println!("\nQuery executed successfully!");
                                                println!("Result shape: {:?}", df.shape());
                                                println!("Columns: {:?}", df.get_column_names());

                                                // Parse VizQL portion
                                                match parser::parse_query(&query) {
                                                    Ok(specs) => {
                                                        println!("\nParsed {} visualization spec(s)", specs.len());

                                                        // Generate visualization output using writer
                                                        #[cfg(feature = "vegalite")]
                                                        if writer == "vegalite" {
                                                            let vl_writer = VegaLiteWriter::new();

                                                            // For now, render the first spec only
                                                            if let Some(spec) = specs.first() {
                                                                match vl_writer.write(spec, &df) {
                                                                    Ok(json_output) => {
                                                                        if let Some(output_path) = &output {
                                                                            // Write to file
                                                                            match std::fs::write(output_path, &json_output) {
                                                                                Ok(_) => println!("\nVega-Lite JSON written to: {}", output_path.display()),
                                                                                Err(e) => {
                                                                                    eprintln!("Failed to write output file: {}", e);
                                                                                    std::process::exit(1);
                                                                                }
                                                                            }
                                                                        } else {
                                                                            // Print to stdout
                                                                            println!("\n{}", json_output);
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        eprintln!("Failed to generate Vega-Lite output: {}", e);
                                                                        std::process::exit(1);
                                                                    }
                                                                }
                                                            } else {
                                                                eprintln!("No visualization specifications found");
                                                                std::process::exit(1);
                                                            }
                                                        }

                                                        #[cfg(not(feature = "vegalite"))]
                                                        {
                                                            if writer == "vegalite" {
                                                                eprintln!("VegaLite writer not compiled in. Rebuild with --features vegalite");
                                                                std::process::exit(1);
                                                            }
                                                        }

                                                        if writer != "vegalite" {
                                                            println!("\nNote: Writer '{}' not yet implemented", writer);
                                                            println!("Available writers: vegalite");
                                                        }
                                                    }
                                                    Err(e) => {
                                                        eprintln!("Failed to parse VizQL portion: {}", e);
                                                        std::process::exit(1);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Failed to execute SQL query: {}", e);
                                                std::process::exit(1);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to create DuckDB reader: {}", e);
                                        std::process::exit(1);
                                    }
                                }
                            } else {
                                eprintln!("Unsupported reader: {}", reader);
                                eprintln!("Currently only 'duckdb://' readers are supported");
                                std::process::exit(1);
                            }

                            #[cfg(not(feature = "duckdb"))]
                            {
                                eprintln!("No reader support compiled in. Rebuild with --features duckdb");
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to split query: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to read file {}: {}", file.display(), e);
                    std::process::exit(1);
                }
            }
        }

        Commands::Parse { query, format } => {
            println!("Parsing query: {}", query);
            println!("Format: {}", format);
            // TODO: Implement parsing logic
            match parser::parse_query(&query) {
                Ok(specs) => {
                    match format.as_str() {
                        "json" => println!("{}", serde_json::to_string_pretty(&specs)?),
                        "debug" => println!("{:#?}", specs),
                        "pretty" => {
                            println!("VizQL Specifications: {} total", specs.len());
                            for (i, spec) in specs.iter().enumerate() {
                                println!("\nVisualization #{} ({:?}):", i + 1, spec.viz_type);
                                println!("  Layers: {}", spec.layers.len());
                                println!("  Scales: {}", spec.scales.len());
                                if spec.facet.is_some() {
                                    println!("  Faceting: Yes");
                                }
                                if spec.theme.is_some() {
                                    println!("  Theme: Yes");
                                }
                            }
                        }
                        _ => {
                            eprintln!("Unknown format: {}", format);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Parse error: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Commands::Validate { query, reader } => {
            println!("Validating query: {}", query);
            if let Some(reader) = reader {
                println!("Reader: {}", reader);
            }
            // TODO: Implement validation logic
            println!("Validation not yet implemented");
        }
    }

    Ok(())
}