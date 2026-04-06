//! Mechanical repo analysis (E10-S2, W-100 Layer 1).
//!
//! Analyzes a workspace directory to detect languages, build commands,
//! test commands, and formatting configuration. Purely factual.

use std::path::Path;

/// Result of mechanical repo analysis.
#[derive(Debug, Clone, Default)]
pub struct RepoAnalysis {
    /// Detected programming languages.
    pub languages: Vec<String>,
    /// Inferred build command.
    pub build_command: Option<String>,
    /// Inferred test command.
    pub test_command: Option<String>,
    /// Inferred lint command.
    pub lint_command: Option<String>,
    /// Detected formatter/linter configuration summary.
    pub formatter_config: String,
    /// Commands extracted from CI configuration.
    pub ci_commands: Vec<String>,
}

impl RepoAnalysis {
    /// Render the analysis as a human-readable string for context injection.
    pub fn render(&self) -> String {
        if self.languages.is_empty() {
            return String::new();
        }

        let mut out = String::new();
        out.push_str(&format!("Languages: {}\n", self.languages.join(", ")));
        if let Some(build) = &self.build_command {
            out.push_str(&format!("Build: {build}\n"));
        }
        if let Some(test) = &self.test_command {
            out.push_str(&format!("Test: {test}\n"));
        }
        if let Some(lint) = &self.lint_command {
            out.push_str(&format!("Lint: {lint}\n"));
        }
        if !self.formatter_config.is_empty() {
            out.push_str(&format!("Formatting: {}\n", self.formatter_config));
        }
        if !self.ci_commands.is_empty() {
            out.push_str("CI commands:\n");
            for command in &self.ci_commands {
                out.push_str(&format!("  - {command}\n"));
            }
        }
        out
    }
}

/// Analyze a repository root and infer factual coding commands/configuration.
pub fn analyze_repo(root: &Path) -> RepoAnalysis {
    let mut analysis = RepoAnalysis::default();

    if root.join("Cargo.toml").exists() {
        analysis.languages.push("Rust".into());
        analysis.build_command = Some("cargo build --workspace".into());
        analysis.test_command = Some("cargo test --workspace".into());
        analysis.lint_command = Some("cargo clippy --all-targets -- -D warnings".into());
    }

    if root.join("package.json").exists() {
        analysis.languages.push("JavaScript/TypeScript".into());
        if let Ok(package_json) = std::fs::read_to_string(root.join("package.json")) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&package_json) {
                if let Some(scripts) = value.get("scripts").and_then(|s| s.as_object()) {
                    if analysis.build_command.is_none() {
                        analysis.build_command = scripts
                            .get("build")
                            .and_then(|v| v.as_str())
                            .map(|s| format!("npm run build  # {s}"));
                    }
                    if analysis.test_command.is_none() {
                        analysis.test_command = scripts
                            .get("test")
                            .and_then(|v| v.as_str())
                            .map(|s| format!("npm test  # {s}"));
                    }
                    if analysis.lint_command.is_none() {
                        analysis.lint_command = scripts
                            .get("lint")
                            .and_then(|v| v.as_str())
                            .map(|s| format!("npm run lint  # {s}"));
                    }
                }
            }
        }
    }

    if root.join("pyproject.toml").exists() || root.join("requirements.txt").exists() {
        analysis.languages.push("Python".into());
        analysis
            .build_command
            .get_or_insert_with(|| "python -m build".into());
        analysis.test_command.get_or_insert_with(|| "pytest".into());
    }

    if root.join("go.mod").exists() {
        analysis.languages.push("Go".into());
        analysis
            .build_command
            .get_or_insert_with(|| "go build ./...".into());
        analysis
            .test_command
            .get_or_insert_with(|| "go test ./...".into());
    }

    analysis.languages.sort();
    analysis.languages.dedup();
    analysis.formatter_config = detect_formatter_config(root);
    analysis.ci_commands = extract_ci_commands(root);
    analysis
}

fn detect_formatter_config(root: &Path) -> String {
    let mut found = Vec::new();

    for filename in ["rustfmt.toml", ".rustfmt.toml"] {
        let path = root.join(filename);
        if path.exists() {
            let preview = std::fs::read_to_string(&path)
                .ok()
                .map(|content| summarize_config(&content))
                .unwrap_or_default();
            found.push(format!("rustfmt ({filename}): {preview}"));
        }
    }

    for filename in [
        ".prettierrc",
        ".prettierrc.json",
        ".editorconfig",
        ".eslintrc",
        ".eslintrc.js",
    ] {
        let path = root.join(filename);
        if path.exists() {
            found.push(filename.to_string());
        }
    }

    found.join("; ")
}

fn summarize_config(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .take(2)
        .collect::<Vec<_>>()
        .join(", ")
}

fn extract_ci_commands(root: &Path) -> Vec<String> {
    let workflows_dir = root.join(".github/workflows");
    let mut commands = Vec::new();

    if let Ok(entries) = std::fs::read_dir(workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(command) = trimmed.strip_prefix("- run:") {
                        commands.push(command.trim().to_string());
                    }
                }
            }
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detect_rust_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(analysis.languages.contains(&"Rust".to_string()));
        assert!(analysis.build_command.is_some());
        assert!(analysis.build_command.unwrap().contains("cargo"));
    }

    #[test]
    fn detect_node_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"test","scripts":{"build":"tsc","test":"jest"}}"#,
        )
        .unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(analysis
            .languages
            .contains(&"JavaScript/TypeScript".to_string()));
        assert!(analysis.test_command.is_some());
    }

    #[test]
    fn detect_python_from_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"test\"\n",
        )
        .unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(analysis.languages.contains(&"Python".to_string()));
    }

    #[test]
    fn detect_go_from_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.21\n",
        )
        .unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(analysis.languages.contains(&"Go".to_string()));
        assert!(analysis.build_command.is_some());
    }

    #[test]
    fn detect_formatter_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("rustfmt.toml"),
            "max_width = 100\ntab_spaces = 4\n",
        )
        .unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(!analysis.formatter_config.is_empty());
        assert!(analysis.formatter_config.contains("rustfmt"));
    }

    #[test]
    fn detect_ci_commands() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        fs::write(
            dir.path().join(".github/workflows/ci.yml"),
            "name: CI\non: push\njobs:\n  test:\n    steps:\n      - run: cargo test --workspace\n      - run: cargo clippy -- -D warnings\n",
        )
        .unwrap();

        let analysis = analyze_repo(dir.path());
        assert!(!analysis.ci_commands.is_empty());
    }

    #[test]
    fn render_analysis_output() {
        let analysis = RepoAnalysis {
            languages: vec!["Rust".into()],
            build_command: Some("cargo build --workspace".into()),
            test_command: Some("cargo test --workspace".into()),
            lint_command: Some("cargo clippy --all -- -D warnings".into()),
            formatter_config: "rustfmt.toml: max_width=100, tab_spaces=4".into(),
            ci_commands: vec!["cargo test --workspace".into()],
        };
        let rendered = analysis.render();
        assert!(rendered.contains("Rust"));
        assert!(rendered.contains("cargo build"));
        assert!(rendered.contains("cargo test"));
        assert!(rendered.contains("clippy"));
    }

    #[test]
    fn empty_directory_produces_empty_analysis() {
        let dir = tempfile::tempdir().unwrap();
        let analysis = analyze_repo(dir.path());
        assert!(analysis.languages.is_empty());
        assert!(analysis.build_command.is_none());
        assert!(analysis.render().is_empty());
    }
}
