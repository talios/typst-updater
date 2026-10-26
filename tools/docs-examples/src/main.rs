//! Generates the data behind the documentation site's examples from `src/main.rs`:
//! the unit-test fixtures, and the real terminal output of a run built from them.

mod fixtures;
mod terminal;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Serialize;

const SOURCE: &str = "src/main.rs";
const OUTPUT_DIR: &str = "docs/src/data";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let root = workspace_root();
    let source = root.join(SOURCE);
    let code = fs::read_to_string(&source)
        .with_context(|| format!("failed to read {}", source.display()))?;
    let file = syn::parse_file(&code).with_context(|| format!("failed to parse {SOURCE}"))?;

    let fixtures = fixtures::extract(&file)?;
    write_json(&root, "examples.json", &fixtures)?;

    let index_url = fixtures::string_const(&file, "PACKAGE_INDEX_URL")?;
    let terminals = terminal::capture(&root, &fixtures, &index_url)?;
    write_json(&root, "terminal.json", &terminals)?;

    println!(
        "Wrote {} test fixture(s) and the terminal output to {OUTPUT_DIR}",
        fixtures.len()
    );
    Ok(())
}

fn write_json(root: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let path = root.join(OUTPUT_DIR).join(name);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("failed to write {}", path.display()))
}

/// The repository root, two levels above this crate.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_real_tests() {
        let code = fs::read_to_string(workspace_root().join(SOURCE)).unwrap();
        let file = syn::parse_file(&code).unwrap();
        let fixtures = fixtures::extract(&file).unwrap();
        assert!(
            fixtures["finds_imports_with_line_numbers"]
                .strings
                .contains_key("src")
        );
        assert!(fixtures::string_const(&file, "PACKAGE_INDEX_URL").is_ok());
    }
}
