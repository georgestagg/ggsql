# VizQL - SQL Visualization Grammar

A SQL extension for declarative data visualization based on the Grammar of Graphics.

VizQL allows you to write queries that combine SQL data retrieval with visualization specifications in a single, composable syntax.

## Example

```sql
SELECT date, revenue, region
FROM sales
WHERE year = 2024
VISUALISE AS PLOT
WITH line USING
    x = date,
    y = revenue,
    color = region
LABELS
    title = 'Sales by Region'
THEME minimal
```

## Project Status

🚧 **Early Development** - Core parsing infrastructure is implemented but not yet feature-complete.

**Completed:**
- ✅ Rust workspace setup with tree-sitter grammar
- ✅ Complete AST type definitions for VizQL specification
- ✅ Basic regex-based query splitter (SQL from VISUALISE portions)
- ✅ Comprehensive test suite for core AST types (6 tests passing)
- ✅ Build system working with proper dependency management

**In Progress:**
- 🔄 Tree-sitter external scanner integration (currently consumes entire input as SQL)
- 🔄 AST builder (tree-sitter CST → typed AST conversion - stub implementation)

**Known Issues:**
- Tree-sitter grammar tests failing (external scanner needs refinement)
- Parser integration tests failing (expected during development)
- Various unused variable warnings (normal for stub implementations)

**Planned:**
- 📋 Reader layer (DuckDB, PostgreSQL, CSV data sources)
- 📋 Writer layer (ggplot2, Vega-Lite, PNG output formats)
- 📋 CLI tool and execution engine

## Architecture

VizQL splits queries at the `VISUALISE AS` boundary:
- **SQL portion** → passed to pluggable readers (DuckDB, PostgreSQL, CSV, etc.)
- **VISUALISE portion** → parsed and compiled into visualization specifications
- **Output** → rendered via pluggable writers (ggplot2, PNG, Vega-Lite, etc.)

## Development Setup

### Prerequisites

- **Rust** 1.70+ with Cargo
- **Node.js** 16+ (for tree-sitter CLI)
- **Git**

### Getting Started

1. **Clone the repository:**
   ```bash
   git clone https://github.com/vizql/vizql
   cd vizql
   ```

2. **Install tree-sitter CLI:**
   ```bash
   npm install -g tree-sitter-cli
   ```

3. **Build the project:**
   ```bash
   cargo build
   ```

   Note: If you encounter dependency conflicts, you can build with minimal features:
   ```bash
   cargo build --no-default-features
   ```

4. **Run tests:**
   ```bash
   cargo test
   ```

## Project Structure

```
vizql/
├── Cargo.toml                       # Workspace root configuration
├── README.md                        # This file
├── CLAUDE.md                        # Complete specification document
│
├── tree-sitter-vizql/               # Tree-sitter grammar package
│   ├── grammar.js                   # Grammar definition
│   ├── src/
│   │   ├── parser.c                 # Generated parser
│   │   ├── scanner.c                # Custom SQL scanner
│   │   └── node-types.json          # Generated node types
│   └── queries/
│       └── highlights.scm           # Syntax highlighting rules
│
└── src/                             # Main library
    ├── lib.rs                       # Public API and re-exports
    ├── cli.rs                       # Command-line interface (disabled)
    │
    └── parser/                      # Parsing subsystem
        ├── mod.rs                   # Parser public API
        ├── ast.rs                   # AST type definitions
        ├── splitter.rs              # Query splitting logic
        ├── builder.rs               # CST → AST conversion
        └── error.rs                 # Parse error types
```

## Development Workflow

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test modules
cargo test ast                       # AST type tests
cargo test splitter                  # Query splitter tests
cargo test parser                    # All parser tests

# Run without default features (avoids database dependencies)
cargo test --no-default-features

# Run with specific features
cargo test --features duckdb,sqlite
```

### Working with the Grammar

The tree-sitter grammar is in `tree-sitter-vizql/grammar.js`. After making changes:

1. **Regenerate the parser:**
   ```bash
   cd tree-sitter-vizql
   tree-sitter generate
   ```

2. **Test the grammar:**
   ```bash
   # Test parsing a specific file
   tree-sitter parse test/corpus/basic.txt

   # Test all corpus files
   tree-sitter test
   ```

3. **Debug parsing issues:**
   ```bash
   # Enable debug mode
   tree-sitter parse --debug test/corpus/basic.txt

   # Check for conflicts
   tree-sitter generate --report-states-for-rule=query
   ```

### Code Organization

- **AST Types** (`src/parser/ast.rs`): Core data structures representing parsed VizQL
- **Query Splitter** (`src/parser/splitter.rs`): Separates SQL from VISUALISE portions
- **AST Builder** (`src/parser/builder.rs`): Converts tree-sitter parse trees to typed AST
- **Error Handling** (`src/parser/error.rs`): Parse-time error types and formatting

### Adding New Grammar Features

1. **Update the grammar** in `tree-sitter-vizql/grammar.js`
2. **Add corresponding AST types** in `src/parser/ast.rs`
3. **Update the AST builder** in `src/parser/builder.rs`
4. **Add test cases** for the new feature
5. **Update syntax highlighting** in `tree-sitter-vizql/queries/highlights.scm`

## Testing Strategy

### Unit Tests
Located alongside the code they test:
- `src/parser/ast.rs` - AST type functionality and validation
- `src/parser/splitter.rs` - Query splitting edge cases
- `src/parser/builder.rs` - CST to AST conversion

### Integration Tests
- Full parsing pipeline tests in `src/parser/mod.rs`
- End-to-end query processing (planned)

### Grammar Tests
- `tree-sitter-vizql/test/corpus/` - Example queries with expected parse trees
- Run with `tree-sitter test`

### Running Specific Test Categories

```bash
# Core AST functionality
cargo test ast::tests

# Query splitting logic
cargo test splitter::tests

# Tree-sitter grammar
cd tree-sitter-vizql && tree-sitter test

# All parser integration tests
cargo test parser
```

## Grammar Specification

See [CLAUDE.md](CLAUDE.md) for the complete VizQL grammar specification, including:
- Complete syntax reference with examples
- AST structure documentation
- Implementation phases and architecture
- Design principles and philosophy

Key grammar elements:
- `VISUALISE AS PLOT` - Entry point for visualization
- `WITH <geom> USING` - Define geometric layers (point, line, bar, etc.)
- `SCALE <aesthetic> USING` - Configure data-to-visual mappings
- `FACET` - Create small multiples
- `LABELS`, `THEME`, `GUIDE` - Styling and annotation

## Contributing

1. **Check existing issues** and project status
2. **Write tests first** for new functionality
3. **Update documentation** for API changes
4. **Follow Rust conventions** and run `cargo fmt`
5. **Ensure all tests pass** before submitting PRs

### Code Style
- Use `cargo fmt` for consistent formatting
- Follow Rust naming conventions (snake_case, etc.)
- Add comprehensive doc comments for public APIs
- Include examples in documentation where helpful

## Debugging Tips

### Parser Issues
1. **Test grammar in isolation:**
   ```bash
   cd tree-sitter-vizql
   echo "SELECT x FROM data VISUALISE AS PLOT WITH point x = x, y = y" | tree-sitter parse
   ```

2. **Check parse tree structure:**
   ```bash
   tree-sitter parse --debug test/corpus/basic.txt
   ```

3. **Validate AST building:**
   ```bash
   cargo test parser::mod::tests::test_simple_query_parsing -- --nocapture
   ```

### Common Issues

**Build Failures:**
- **Dependency conflicts**: Use `cargo build --no-default-features` to build with minimal dependencies
- **Missing modules**: Some advanced modules (readers, writers, engine) are not yet implemented and are commented out

**Grammar Issues:**
- **Grammar conflicts**: Check `tree-sitter generate` output for conflicts
- **C compilation errors**: Usually in `src/scanner.c`, check includes and syntax
- **Parse failures**: Add debug prints in AST builder to trace node walking

**Development:**
- **Unused variable warnings**: Expected during development - these are stub implementations
- **Tree-sitter scanner warnings**: The C scanner has some unused parameters - this is normal

## License

MIT OR Apache-2.0

## Links

- **Specification**: [CLAUDE.md](CLAUDE.md) - Complete grammar and implementation guide
- **Grammar**: [tree-sitter-vizql/grammar.js](tree-sitter-vizql/grammar.js) - Tree-sitter grammar definition
- **Examples**: [tree-sitter-vizql/test/corpus/](tree-sitter-vizql/test/corpus/) - Example queries and parse trees

---

**Note**: This project is in early development. The API is not yet stable and breaking changes are expected as we implement the full specification.