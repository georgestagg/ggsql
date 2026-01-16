use std::{env, fs};

static PENGUINS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../data/penguins.parquet"
));

pub fn prep_penguins_query() -> String {
    let mut tmp_path = env::temp_dir();
    tmp_path.push("penguins.parquet");
    if !tmp_path.exists() {
        fs::write(&tmp_path, PENGUINS).expect("Failed to write penguins dataset.");
    }
    let out = format!(
        "CREATE TABLE 'penguins' AS SELECT * FROM read_parquet('{}')",
        tmp_path.display()
    );
    out
}
