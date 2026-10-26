//! Runs the real typst-updater binary against a scratch project built from the test
//! fixtures, and captures its coloured output for the site's terminal windows.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::{env, fs, thread};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

use crate::fixtures::Fixture;

/// The project the sample run updates: the files the directory-walk test creates,
/// filled with the sources of the tests named here (any other file is left empty).
const WALK_TEST: &str = "finds_typ_files_recursively_skipping_hidden_dirs";
const FILE_CONTENTS: &[(&str, &str)] = &[
    (
        "main.typ",
        "rewrite_changes_only_versions_and_handles_length_changes",
    ),
    ("chapters/ch1.typ", "finds_multiline_and_code_mode_imports"),
];
/// The package index served to the sample run: the latest versions the rewrite test uses.
const INDEX_TEST: &str = "rewrite_changes_only_versions_and_handles_length_changes";
const PROJECT: &str = "my-thesis";

#[derive(Serialize)]
pub(crate) struct Terminals {
    help: Session,
    run: Session,
}

/// One captured command: what was typed and what it printed.
#[derive(Serialize)]
struct Session {
    cwd: String,
    command: String,
    output: Vec<Span>,
}

/// A run of output text sharing the same styles (`b`, `dim`, `u`, `red`, `green`, `cyan`).
#[derive(Serialize, PartialEq, Debug)]
struct Span {
    text: String,
    styles: Vec<&'static str>,
}

pub(crate) fn capture(
    root: &Path,
    fixtures: &BTreeMap<String, Fixture>,
    index_url: &str,
) -> Result<Terminals> {
    let fixture = |name: &str| {
        fixtures
            .get(name)
            .with_context(|| format!("no test named {name}"))
    };

    let dir = tempfile::tempdir()?;
    let project = dir.path().join(PROJECT);
    for file in &fixture(WALK_TEST)?.files {
        let contents = match FILE_CONTENTS.iter().find(|(path, _)| path == file) {
            Some((_, test)) => fixture(test)?
                .strings
                .get("src")
                .with_context(|| format!("test {test} has no `src` fixture"))?
                .as_str(),
            None => "",
        };
        let path = project.join(file);
        fs::create_dir_all(path.parent().expect("the path was joined onto a directory"))?;
        fs::write(&path, contents)?;
    }

    let help = run(root, &project, &["--help"], "")?;

    let entries: Vec<_> = fixture(INDEX_TEST)?
        .latest
        .iter()
        .map(|(name, version)| json!({ "name": name, "version": version }))
        .collect();
    let local_url = serve_once(serde_json::to_string(&entries)?)?;
    let output = run(root, &project, &["--index-url", &local_url], "y\n")?
        .replace(&local_url, index_url)
        // A terminal echoes the answer typed at the prompt.
        .replace("[y/N] ", "[y/N] y\n");

    let cwd = format!("~/{PROJECT}");
    Ok(Terminals {
        help: Session {
            cwd: cwd.clone(),
            command: "typst-updater --help".into(),
            output: spans(&help),
        },
        run: Session {
            cwd,
            command: "typst-updater".into(),
            output: spans(&output),
        },
    })
}

/// Runs typst-updater from `project` with colours forced on, returning its stdout.
fn run(root: &Path, project: &Path, args: &[&str], stdin: &str) -> Result<String> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut child = Command::new(cargo)
        .args([
            "run",
            "--quiet",
            "--locked",
            "-p",
            "typst-updater",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .arg("--")
        .args(args)
        .current_dir(project)
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run typst-updater")?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "typst-updater {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// Serves `body` as JSON for a single request on a local port, returning its URL.
fn serve_once(body: String) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/index.json", listener.local_addr()?);
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        // Read the request up to the blank line that ends its headers.
        let mut line = String::new();
        let mut reader = BufReader::new(&stream);
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
            line.clear();
        }
        let _ = write!(
            &stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    Ok(url)
}

/// Splits ANSI-styled text into spans, keeping the styles the site renders.
fn spans(ansi: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut styles: Vec<&'static str> = Vec::new();
    let mut push = |text: &str, styles: &[&'static str]| {
        if text.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.styles == styles => last.text.push_str(text),
            _ => spans.push(Span {
                text: text.into(),
                styles: styles.to_vec(),
            }),
        }
    };

    let mut rest = ansi;
    while let Some(start) = rest.find("\x1b[") {
        push(&rest[..start], &styles);
        let sequence = &rest[start + 2..];
        let Some(end) = sequence.find('m') else { break };
        let mut codes = sequence[..end].split(';');
        while let Some(code) = codes.next() {
            let style = match code {
                "" | "0" => {
                    styles.clear();
                    continue;
                }
                // Extended colours carry their own arguments: `5;n` or `2;r;g;b`.
                "38" | "48" => {
                    let skip = if codes.next() == Some("5") { 1 } else { 3 };
                    codes.nth(skip - 1);
                    continue;
                }
                "1" => "b",
                "2" => "dim",
                "4" => "u",
                "31" => "red",
                "32" => "green",
                "36" => "cyan",
                _ => continue,
            };
            if !styles.contains(&style) {
                styles.push(style);
            }
        }
        rest = &sequence[end + 1..];
    }
    push(rest, &styles);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, styles: &[&'static str]) -> Span {
        Span {
            text: text.into(),
            styles: styles.to_vec(),
        }
    }

    #[test]
    fn splits_ansi_styles_into_spans() {
        assert_eq!(
            spans("plain \x1b[1;36mfile\x1b[0m \x1b[31m0.1.0\x1b[0m → \x1b[32m0.2.0\x1b[0m\n"),
            [
                span("plain ", &[]),
                span("file", &["b", "cyan"]),
                span(" ", &[]),
                span("0.1.0", &["red"]),
                span(" → ", &[]),
                span("0.2.0", &["green"]),
                span("\n", &[]),
            ]
        );
    }

    #[test]
    fn merges_adjacent_spans_and_drops_unknown_codes() {
        assert_eq!(
            spans("\x1b[1m\x1b[4mUsage:\x1b[0m\x1b[0m a\x1b[38;5;1mb"),
            [span("Usage:", &["b", "u"]), span(" ab", &[])]
        );
    }
}
