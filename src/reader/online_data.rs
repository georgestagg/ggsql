use crate::{DataFrame, GgsqlError};

// =============================================================================
// Dataset registry
// =============================================================================

/// Resolve an online dataset name to its download URL.
pub fn resolve_online_dataset(name: &str) -> Option<&'static str> {
    let name = name.replace('-', "_");
    let url = match name.as_str() {
        "world" | "world_110m" | "countries" | "countries_110m" => {
            "https://example.com/placeholder/ne_110m_admin_0_countries.parquet"
        }
        "world_50m" | "countries_50m" => {
            "https://example.com/placeholder/ne_50m_admin_0_countries.parquet"
        }
        "world_10m" | "countries_10m" => {
            "https://example.com/placeholder/ne_10m_admin_0_countries.parquet"
        }
        "states" | "states_110m" | "provinces" | "provinces_110m" => {
            "https://example.com/placeholder/ne_110m_admin_1_states_provinces.parquet"
        }
        "states_50m" | "provinces_50m" => {
            "https://example.com/placeholder/ne_50m_admin_1_states_provinces.parquet"
        }
        "states_10m" | "provinces_10m" => {
            "https://example.com/placeholder/ne_10m_admin_1_states_provinces.parquet"
        }
        "us_counties" | "us_counties_10m" => {
            "https://example.com/placeholder/ne_10m_admin_2_counties.parquet"
        }
        _ => return None,
    };
    Some(url)
}

// =============================================================================
// Native download + cache (not available on wasm32)
// =============================================================================

#[cfg(all(not(target_arch = "wasm32"), feature = "parquet"))]
mod native {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    pub(super) fn cache_dir() -> Result<PathBuf, GgsqlError> {
        let dir = platform_cache_dir().join("ggsql").join("online");
        fs::create_dir_all(&dir).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to create cache directory '{}': {}",
                dir.display(),
                e
            ))
        })?;
        Ok(dir)
    }

    fn platform_cache_dir() -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            if let Ok(dir) = std::env::var("XDG_CACHE_HOME") {
                return PathBuf::from(dir);
            }
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(".cache");
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join("Library").join("Caches");
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(dir) = std::env::var("LOCALAPPDATA") {
                return PathBuf::from(dir);
            }
        }

        std::env::temp_dir()
    }

    fn cache_filename(url: &str) -> String {
        url.rsplit('/')
            .next()
            .unwrap_or("dataset.parquet")
            .to_string()
    }

    fn ensure_downloaded(url: &str) -> Result<PathBuf, GgsqlError> {
        let filename = cache_filename(url);
        let parquet_path = cache_dir()?.join(&filename);

        if parquet_path.exists() {
            return Ok(parquet_path);
        }

        let partial_path = parquet_path.with_extension("parquet.partial");

        // Clean up prior failed attempt
        let _ = fs::remove_file(&partial_path);

        let response = ureq::get(url).call().map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to download '{}': {}. Are you connected to the internet?",
                url, e
            ))
        })?;

        let bytes = response.into_body().read_to_vec().map_err(|e| {
            GgsqlError::ReaderError(format!("Failed to read response from '{}': {}", url, e))
        })?;

        fs::write(&partial_path, &bytes).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to write cache file '{}': {}",
                partial_path.display(),
                e
            ))
        })?;

        fs::rename(&partial_path, &parquet_path).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to finalize cache file '{}': {}",
                parquet_path.display(),
                e
            ))
        })?;

        Ok(parquet_path)
    }

    /// Load an online dataset by name, downloading and caching as needed.
    pub fn load_online_dataframe(name: &str) -> Result<DataFrame, GgsqlError> {
        let url = resolve_online_dataset(name).ok_or_else(|| {
            GgsqlError::ReaderError(format!("Unknown online dataset: '{}'", name))
        })?;

        let parquet_path = ensure_downloaded(url)?;

        let bytes = fs::read(&parquet_path).map_err(|e| {
            GgsqlError::ReaderError(format!(
                "Failed to read cached parquet '{}': {}",
                parquet_path.display(),
                e
            ))
        })?;

        crate::reader::builtin_data::dataframe_from_parquet_bytes(name, bytes::Bytes::from(bytes))
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "parquet"))]
pub use native::load_online_dataframe;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_world() {
        let url = resolve_online_dataset("world").unwrap();
        assert!(url.contains("110m_admin_0_countries"));
    }

    #[test]
    fn test_resolve_world_50m() {
        let url = resolve_online_dataset("world_50m").unwrap();
        assert!(url.contains("50m_admin_0_countries"));
    }

    #[test]
    fn test_resolve_world_10m() {
        let url = resolve_online_dataset("world_10m").unwrap();
        assert!(url.contains("10m_admin_0_countries"));
    }

    #[test]
    fn test_resolve_countries_alias() {
        let world = resolve_online_dataset("world").unwrap();
        let countries = resolve_online_dataset("countries").unwrap();
        assert_eq!(world, countries);
    }

    #[test]
    fn test_resolve_states() {
        let url = resolve_online_dataset("states").unwrap();
        assert!(url.contains("110m_admin_1_states_provinces"));
    }

    #[test]
    fn test_resolve_provinces_alias() {
        let states = resolve_online_dataset("states").unwrap();
        let provinces = resolve_online_dataset("provinces").unwrap();
        assert_eq!(states, provinces);
    }

    #[test]
    fn test_resolve_us_counties() {
        let url = resolve_online_dataset("us_counties").unwrap();
        assert!(url.contains("10m_admin_2_counties"));
    }

    #[test]
    fn test_resolve_us_counties_hyphen() {
        let url = resolve_online_dataset("us-counties").unwrap();
        assert!(url.contains("10m_admin_2_counties"));
    }

    #[test]
    fn test_resolve_unknown() {
        assert!(resolve_online_dataset("nonexistent").is_none());
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "parquet"))]
    mod native_tests {
        use super::super::native;

        #[test]
        fn test_cache_dir_exists() {
            let dir = native::cache_dir().unwrap();
            assert!(dir.exists());
        }
    }
}
