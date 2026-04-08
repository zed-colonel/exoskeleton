//! Dataset fetching from HuggingFace REST API with local JSONL caching.

use std::path::{Path, PathBuf};

use exoskeleton_core::ExoError;

use super::{DatasetSource, SweInstance};

/// HuggingFace Rows API base URL.
const HF_ROWS_API: &str = "https://datasets-server.huggingface.co/rows";

/// Page size for HuggingFace API requests.
const PAGE_SIZE: u64 = 100;

/// Known Rust repos in SWE-bench Multilingual (for filtering).
const MULTILINGUAL_RUST_REPOS: &[&str] = &[
    "astral-sh/ruff",
    "burntsushi/ripgrep",
    "nushell/nushell",
    "sharkdp/bat",
    "tokio-rs/axum",
    "tokio-rs/tokio",
    "uutils/coreutils",
];

/// Fetch a SWE-bench dataset, using local cache if available.
pub fn fetch_dataset(
    source: DatasetSource,
    cache_dir: &Path,
    refresh: bool,
) -> Result<Vec<SweInstance>, ExoError> {
    let cache_file = cache_path(source, cache_dir);

    if !refresh {
        if let Ok(instances) = load_from_cache(&cache_file) {
            tracing::info!(
                count = instances.len(),
                cache = %cache_file.display(),
                "loaded dataset from cache"
            );
            return Ok(instances);
        }
    }

    tracing::info!(source = ?source, "fetching dataset from HuggingFace");
    let instances = fetch_from_hf(source)?;

    if let Err(e) = save_to_cache(&instances, &cache_file) {
        tracing::warn!(error = %e, "failed to cache dataset");
    }

    Ok(instances)
}

fn cache_path(source: DatasetSource, cache_dir: &Path) -> PathBuf {
    let name = match source {
        DatasetSource::RustBench => "rustbench.jsonl",
        DatasetSource::Multilingual => "multilingual-rust.jsonl",
    };
    cache_dir.join(name)
}

fn load_from_cache(path: &Path) -> Result<Vec<SweInstance>, ExoError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ExoError::Engine(format!("cache read failed: {e}")))?;
    let mut instances = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let instance: SweInstance = serde_json::from_str(line)
            .map_err(|e| ExoError::Engine(format!("cache parse error: {e}")))?;
        instances.push(instance);
    }
    Ok(instances)
}

fn save_to_cache(instances: &[SweInstance], path: &Path) -> Result<(), ExoError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ExoError::Engine(format!("cache dir creation failed: {e}")))?;
    }
    let tmp_path = path.with_extension("jsonl.tmp");
    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|e| ExoError::Engine(format!("cache tmp file creation failed: {e}")))?;
    for instance in instances {
        let line = serde_json::to_string(instance)
            .map_err(|e| ExoError::Engine(format!("serialization failed: {e}")))?;
        use std::io::Write;
        writeln!(file, "{line}")
            .map_err(|e| ExoError::Engine(format!("cache write failed: {e}")))?;
    }
    std::fs::rename(&tmp_path, path)
        .map_err(|e| ExoError::Engine(format!("cache rename failed: {e}")))?;
    Ok(())
}

fn fetch_from_hf(source: DatasetSource) -> Result<Vec<SweInstance>, ExoError> {
    let (dataset, config, split) = match source {
        DatasetSource::RustBench => ("user2f86/rustbench", "default", "train"),
        DatasetSource::Multilingual => ("SWE-bench/SWE-bench_Multilingual", "default", "test"),
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| ExoError::Engine(format!("HTTP client build failed: {e}")))?;

    let mut all_instances = Vec::new();
    let mut offset: u64 = 0;

    loop {
        tracing::debug!(offset, "fetching page");

        let response: serde_json::Value = client
            .get(HF_ROWS_API)
            .query(&[
                ("dataset", dataset),
                ("config", config),
                ("split", split),
                ("offset", &offset.to_string()),
                ("length", &PAGE_SIZE.to_string()),
            ])
            .send()
            .map_err(|e| ExoError::Engine(format!("HF API request failed: {e}")))?
            .error_for_status()
            .map_err(|e| ExoError::Engine(format!("HF API returned error: {e}")))?
            .json()
            .map_err(|e| ExoError::Engine(format!("HF API response parse failed: {e}")))?;

        let total = response["num_rows_total"]
            .as_u64()
            .ok_or_else(|| ExoError::Engine("missing num_rows_total in HF response".into()))?;

        let rows = response["rows"]
            .as_array()
            .ok_or_else(|| ExoError::Engine("missing rows array in HF response".into()))?;

        if rows.is_empty() {
            break;
        }

        for row_wrapper in rows {
            let row = &row_wrapper["row"];
            match serde_json::from_value::<SweInstance>(row.clone()) {
                Ok(instance) => all_instances.push(instance),
                Err(e) => {
                    let id = row["instance_id"].as_str().unwrap_or("<unknown>");
                    tracing::warn!(
                        instance_id = id,
                        error = %e,
                        "skipping unparseable instance"
                    );
                }
            }
        }

        offset += rows.len() as u64;
        if offset >= total {
            break;
        }

        eprintln!("  fetched {offset}/{total} instances...");
    }

    // Filter Multilingual to Rust only
    if source == DatasetSource::Multilingual {
        all_instances.retain(|i| MULTILINGUAL_RUST_REPOS.iter().any(|r| i.repo == *r));
    }

    tracing::info!(count = all_instances.len(), "dataset fetch complete");
    Ok(all_instances)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_instance() -> SweInstance {
        SweInstance {
            instance_id: "test__repo-1".into(),
            repo: "test/repo".into(),
            base_commit: "abc123".into(),
            problem_statement: "Fix the bug".into(),
            hints_text: String::new(),
            patch: "diff --git a/...".into(),
            test_patch: "diff --git b/...".into(),
            fail_to_pass: vec!["tests::test_fix".into()],
            pass_to_pass: vec!["tests::test_existing".into()],
        }
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("test.jsonl");
        let instances = vec![sample_instance()];

        save_to_cache(&instances, &cache_file).unwrap();
        let loaded = load_from_cache(&cache_file).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].instance_id, "test__repo-1");
        assert_eq!(loaded[0].fail_to_pass, vec!["tests::test_fix"]);
    }

    #[test]
    fn cache_path_rustbench() {
        let dir = PathBuf::from("/tmp/cache");
        assert_eq!(
            cache_path(DatasetSource::RustBench, &dir),
            PathBuf::from("/tmp/cache/rustbench.jsonl")
        );
    }

    #[test]
    fn cache_path_multilingual() {
        let dir = PathBuf::from("/tmp/cache");
        assert_eq!(
            cache_path(DatasetSource::Multilingual, &dir),
            PathBuf::from("/tmp/cache/multilingual-rust.jsonl")
        );
    }
}
