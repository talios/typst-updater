use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::io::{self, BufRead, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
use regex::Regex;
use semver::{Comparator, Op, Version};
use serde::Deserialize;
use typst_syntax::{LinkedNode, SyntaxKind};
use walkdir::{DirEntry, WalkDir};

const PACKAGE_INDEX_URL: &str = "https://packages.typst.org/preview/index.json";
const INDEX_TIMEOUT: Duration = Duration::from_secs(30);

static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\bimport\s+"@preview/([A-Za-z0-9_-]+):([0-9]+\.[0-9]+\.[0-9]+)""#)
        .expect("the import pattern is a valid regex")
});

/// Find Typst package imports and update them to their latest versions.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Directory to search for *.typ files
    #[arg(short, long, default_value = ".")]
    path: PathBuf,

    /// Apply updates without prompting
    #[arg(short, long, visible_alias = "force", short_alias = 'f')]
    yes: bool,

    /// Also update imports in comments and raw blocks, such as the examples in a package's documentation
    #[arg(long)]
    include_docs: bool,

    /// Package index to read instead of packages.typst.org; used to generate the documentation
    #[arg(long, hide = true, default_value = PACKAGE_INDEX_URL)]
    index_url: String,
}

#[derive(Deserialize)]
struct IndexEntry {
    name: String,
    version: String,
}

/// Which `import`s to look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// Only imports that Typst evaluates.
    Code,
    /// Also imports inside comments and raw blocks.
    IncludeDocs,
}

/// A single `#import "@preview/name:version"` occurrence within a file.
struct Import {
    line: usize,
    package: String,
    current: Version,
    /// Byte range of the version text within the file contents.
    version_span: Range<usize>,
}

struct Update {
    import: Import,
    latest: Version,
}

struct FileUpdates {
    path: PathBuf,
    /// Path relative to the search root, for display.
    display: String,
    contents: String,
    updates: Vec<Update>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let scope = if args.include_docs {
        Scope::IncludeDocs
    } else {
        Scope::Code
    };

    let files = find_typst_files(&args.path)?;
    if files.is_empty() {
        println!("No .typ files found in {}", args.path.display());
        return Ok(());
    }
    println!(
        "Scanning {} Typst file(s) in {}",
        files.len(),
        args.path.display().to_string().bold()
    );

    println!("Fetching package index from {}", args.index_url.dimmed());
    let latest_versions = fetch_latest_versions(&args.index_url)?;

    let mut pending = Vec::new();
    for path in files {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let updates = find_updates(&contents, &latest_versions, scope);
        if !updates.is_empty() {
            let display = path
                .strip_prefix(&args.path)
                .unwrap_or(&path)
                .display()
                .to_string();
            pending.push(FileUpdates {
                path,
                display,
                contents,
                updates,
            });
        }
    }

    if pending.is_empty() {
        println!("{}", "All package imports are up to date.".green());
        return Ok(());
    }

    print_updates(&pending);

    let total: usize = pending.iter().map(|f| f.updates.len()).sum();
    if !args.yes && !confirm(&format!("Apply {total} update(s)?"))? {
        println!("No changes made.");
        return Ok(());
    }

    let mut updated_files = 0;
    let mut updated_imports = 0;
    for file in &pending {
        if apply_updates(file)? {
            updated_files += 1;
            updated_imports += file.updates.len();
        }
    }
    println!(
        "\n{} {updated_imports} import(s) across {updated_files} file(s).",
        "Updated".green().bold(),
    );
    Ok(())
}

fn find_typst_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e));
    for entry in walker {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "typ") {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|s| s.starts_with('.'))
}

/// Returns the highest published version of every package in the preview namespace.
fn fetch_latest_versions(url: &str) -> Result<HashMap<String, Version>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(INDEX_TIMEOUT))
        .build()
        .into();
    let entries: Vec<IndexEntry> = agent
        .get(url)
        .call()
        .with_context(|| format!("failed to fetch Typst package index from {url}"))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("failed to parse Typst package index from {url}"))?;
    Ok(latest_versions(entries))
}

fn latest_versions(entries: Vec<IndexEntry>) -> HashMap<String, Version> {
    let mut latest: HashMap<String, Version> = HashMap::new();
    for entry in entries {
        let Ok(version) = Version::parse(&entry.version) else {
            continue;
        };
        match latest.entry(entry.name) {
            Entry::Occupied(mut e) if version > *e.get() => {
                e.insert(version);
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(e) => {
                e.insert(version);
            }
        }
    }
    latest
}

/// Returns the imports in `contents` that have a newer version in `latest`.
fn find_updates(contents: &str, latest: &HashMap<String, Version>, scope: Scope) -> Vec<Update> {
    find_imports(contents, scope)
        .into_iter()
        .filter_map(|import| {
            let newest = latest.get(&import.package)?;
            (*newest > import.current).then(|| Update {
                latest: newest.clone(),
                import,
            })
        })
        .collect()
}

fn find_imports(contents: &str, scope: Scope) -> Vec<Import> {
    let code_imports = (scope == Scope::Code).then(|| import_ranges(contents));
    IMPORT_RE
        .captures_iter(contents)
        .filter_map(|caps| {
            let version = caps.get(2)?;
            if let Some(ranges) = &code_imports
                && !ranges.iter().any(|r| r.contains(&version.start()))
            {
                return None;
            }
            Some(Import {
                line: contents[..version.start()].matches('\n').count() + 1,
                package: caps[1].to_string(),
                current: Version::parse(version.as_str()).ok()?,
                version_span: version.range(),
            })
        })
        .collect()
}

/// Byte ranges of the `import` statements in `contents` that Typst evaluates, which
/// excludes any that only appear in comments or raw blocks.
fn import_ranges(contents: &str) -> Vec<Range<usize>> {
    fn walk(node: &LinkedNode<'_>, ranges: &mut Vec<Range<usize>>) {
        if node.kind() == SyntaxKind::ModuleImport {
            ranges.push(node.range());
            return;
        }
        for child in node.children() {
            walk(&child, ranges);
        }
    }

    let root = typst_syntax::parse(contents);
    let mut ranges = Vec::new();
    walk(&LinkedNode::new(&root), &mut ranges);
    ranges
}

/// Whether moving from `current` to `latest` may break the importing document, using
/// Cargo's caret rules: a new major version, or a new minor (or patch) version while the
/// major (and minor) version is still 0.
fn is_breaking(current: &Version, latest: &Version) -> bool {
    let compatible = Comparator {
        op: Op::Caret,
        major: current.major,
        minor: Some(current.minor),
        patch: Some(current.patch),
        pre: current.pre.clone(),
    };
    !compatible.matches(latest)
}

fn print_updates(files: &[FileUpdates]) {
    let name_width = files
        .iter()
        .flat_map(|f| &f.updates)
        .map(|u| u.import.package.len())
        .max()
        .unwrap_or(0);

    println!("\n{}", "Available updates:".bold());
    for file in files {
        println!("\n  {}", file.display.cyan().bold());
        for update in &file.updates {
            let latest = update.latest.to_string();
            let (latest, note) = if is_breaking(&update.import.current, &update.latest) {
                (latest.yellow(), "  breaking".yellow())
            } else {
                (latest.green(), "".normal())
            };
            println!(
                "    {:>5}  {:<name_width$}  {:>8} → {latest}{note}",
                format!("L{}", update.import.line).dimmed(),
                update.import.package,
                update.import.current.to_string().red(),
            );
        }
    }
    println!();
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn rewrite(contents: &str, updates: &[Update]) -> String {
    let mut contents = contents.to_string();
    // Replace from the end so earlier spans stay valid.
    for update in updates.iter().rev() {
        contents.replace_range(
            update.import.version_span.clone(),
            &update.latest.to_string(),
        );
    }
    contents
}

/// Writes `file`'s updates, returning `false` without touching it if it changed since it was scanned.
fn apply_updates(file: &FileUpdates) -> Result<bool> {
    let current = fs::read_to_string(&file.path)
        .with_context(|| format!("failed to re-read {}", file.path.display()))?;
    if current != file.contents {
        println!(
            "\n  {} {} changed since it was scanned; skipped",
            "!".yellow(),
            file.display.bold()
        );
        return Ok(false);
    }
    write_atomically(&file.path, &rewrite(&file.contents, &file.updates))
        .with_context(|| format!("failed to write {}", file.path.display()))?;

    println!("\n  {} {}", "✔".green(), file.display.bold());
    for update in &file.updates {
        println!(
            "      line {}: @preview/{} {} → {}",
            update.import.line, update.import.package, update.import.current, update.latest,
        );
    }
    Ok(true)
}

/// Replaces `path` through a temporary file in the same directory, so a failed write
/// never leaves it truncated. The file keeps its permissions.
fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(contents.as_bytes())?;
    temp.as_file()
        .set_permissions(fs::metadata(path)?.permissions())?;
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn entry(name: &str, version: &str) -> IndexEntry {
        IndexEntry {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    fn latest(pairs: &[(&str, &str)]) -> HashMap<String, Version> {
        pairs
            .iter()
            .map(|(n, ver)| (n.to_string(), v(ver)))
            .collect()
    }

    #[test]
    fn latest_versions_picks_highest_regardless_of_order() {
        let map = latest_versions(vec![
            entry("cetz", "0.2.0"),
            entry("cetz", "0.10.0"),
            entry("cetz", "0.9.1"),
            entry("tablex", "0.0.9"),
        ]);
        assert_eq!(map["cetz"], v("0.10.0"));
        assert_eq!(map["tablex"], v("0.0.9"));
    }

    #[test]
    fn latest_versions_skips_unparseable_versions() {
        let map = latest_versions(vec![entry("cetz", "0.2.0"), entry("cetz", "not-a-version")]);
        assert_eq!(map["cetz"], v("0.2.0"));
    }

    #[test]
    fn finds_imports_with_line_numbers() {
        let src = "#import \"@preview/cetz:0.1.0\": canvas\n\n#import \"@preview/tablex:0.0.1\"\n";
        let imports = find_imports(src, Scope::Code);
        assert_eq!(imports.len(), 2);
        assert_eq!((imports[0].package.as_str(), imports[0].line), ("cetz", 1));
        assert_eq!(
            (imports[1].package.as_str(), imports[1].line),
            ("tablex", 3)
        );
        assert_eq!(&src[imports[0].version_span.clone()], "0.1.0");
    }

    #[test]
    fn finds_multiline_and_code_mode_imports() {
        let src = "#import \"@preview/headstamp:0.1.0\": (\n  a, b,\n)\n#{ import \"@preview/cetz:0.1.0\" }\n";
        let names: Vec<_> = find_imports(src, Scope::Code)
            .into_iter()
            .map(|i| i.package)
            .collect();
        assert_eq!(names, ["headstamp", "cetz"]);
    }

    #[test]
    fn ignores_non_package_and_non_preview_imports() {
        let src = concat!(
            "#import \"custom-theme.typ\": custom\n",
            "#import \"@local/mine:0.1.0\"\n",
            "#include \"@preview/cetz:0.1.0\"\n",
            "#let s = \"@preview/cetz:0.1.0\"\n",
        );
        assert!(find_imports(src, Scope::Code).is_empty());
    }

    #[test]
    fn skips_imports_in_comments_and_raw_blocks_unless_docs_are_included() {
        let src = concat!(
            "#import \"@preview/cetz:0.1.0\"\n",
            "// #import \"@preview/commented:0.1.0\"\n",
            "/* #import \"@preview/blocked:0.1.0\" */\n",
            "```typ\n",
            "#import \"@preview/example:0.1.0\"\n",
            "```\n",
        );
        let names = |scope| -> Vec<_> {
            find_imports(src, scope)
                .into_iter()
                .map(|i| i.package)
                .collect()
        };
        assert_eq!(names(Scope::Code), ["cetz"]);
        assert_eq!(
            names(Scope::IncludeDocs),
            ["cetz", "commented", "blocked", "example"]
        );
    }

    #[test]
    fn find_updates_only_reports_newer_known_packages() {
        let src = concat!(
            "#import \"@preview/old:0.1.0\"\n",
            "#import \"@preview/current:1.0.0\"\n",
            "#import \"@preview/ahead:2.0.0\"\n",
            "#import \"@preview/unknown:0.1.0\"\n",
        );
        let index = latest(&[("old", "0.2.0"), ("current", "1.0.0"), ("ahead", "1.0.0")]);
        let updates = find_updates(src, &index, Scope::Code);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].import.package, "old");
        assert_eq!(updates[0].latest, v("0.2.0"));
    }

    #[test]
    fn rewrite_changes_only_versions_and_handles_length_changes() {
        let src = concat!(
            "#import \"@preview/cetz:0.1.0\": canvas\n",
            "// keep this\n",
            "#import \"@preview/tablex:0.0.1\"\n",
        );
        let index = latest(&[("cetz", "0.10.12"), ("tablex", "0.0.9")]);
        let out = rewrite(src, &find_updates(src, &index, Scope::Code));
        assert_eq!(
            out,
            concat!(
                "#import \"@preview/cetz:0.10.12\": canvas\n",
                "// keep this\n",
                "#import \"@preview/tablex:0.0.9\"\n",
            )
        );
    }

    #[test]
    fn breaking_updates_follow_caret_rules() {
        assert!(!is_breaking(&v("1.2.3"), &v("1.9.0")));
        assert!(is_breaking(&v("1.2.3"), &v("2.0.0")));
        assert!(!is_breaking(&v("0.2.0"), &v("0.2.5")));
        assert!(is_breaking(&v("0.2.0"), &v("0.3.0")));
        assert!(is_breaking(&v("0.0.1"), &v("0.0.2")));
    }

    fn pending(path: PathBuf, contents: &str) -> FileUpdates {
        let index = latest(&[("cetz", "0.2.0")]);
        FileUpdates {
            path,
            display: "main.typ".into(),
            contents: contents.into(),
            updates: find_updates(contents, &index, Scope::Code),
        }
    }

    #[test]
    fn apply_updates_skips_files_changed_since_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.typ");
        let scanned = "#import \"@preview/cetz:0.1.0\"\n";
        let edited = "#import \"@preview/cetz:0.1.0\"\n= New heading\n";
        fs::write(&path, edited).unwrap();

        assert!(!apply_updates(&pending(path.clone(), scanned)).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    }

    #[cfg(unix)]
    #[test]
    fn apply_updates_rewrites_in_place_keeping_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.typ");
        let src = "#import \"@preview/cetz:0.1.0\"\n";
        fs::write(&path, src).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(apply_updates(&pending(path.clone(), src)).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "#import \"@preview/cetz:0.2.0\"\n"
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn finds_typ_files_recursively_skipping_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("chapters")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        for f in ["main.typ", "chapters/ch1.typ", ".git/x.typ", "notes.txt"] {
            fs::write(root.join(f), "").unwrap();
        }
        let found: Vec<_> = find_typst_files(root)
            .unwrap()
            .into_iter()
            .map(|p| p.strip_prefix(root).unwrap().to_path_buf())
            .collect();
        assert_eq!(
            found,
            [PathBuf::from("chapters/ch1.typ"), PathBuf::from("main.typ")]
        );
    }
}
