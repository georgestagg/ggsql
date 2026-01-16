use std::{env, fs};

static PENGUINS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../data/penguins.parquet"
));

static AIRQUALITY: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../data/airquality.parquet"
));

pub fn prep_penguins_query() -> String {
    prep_builtin_dataset_query("penguins", PENGUINS)
}

pub fn prep_airquality_query() -> String {
    prep_builtin_dataset_query("airquality", AIRQUALITY)
}

fn prep_builtin_dataset_query(name: &str, data: &[u8]) -> String {
    let mut tmp_path = env::temp_dir();
    let mut filename = name.to_string();
    filename.push_str(".parquet");
    tmp_path.push(filename);
    if !tmp_path.exists() {
        fs::write(&tmp_path, data).expect("Failed to write dataset");
    }
    format!(
        "CREATE TABLE '{}' AS SELECT * FROM read_parquet('{}')",
        name,
        tmp_path.display()
    )
}
