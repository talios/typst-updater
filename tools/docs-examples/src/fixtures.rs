//! Extracts the fixtures from the unit tests in `src/main.rs`, so the documentation
//! examples are generated from the same data the tests check.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Expr, File, Item, Lit, Pat, Token};

/// Everything the site can use from one `#[test]` function.
#[derive(Serialize, Default, PartialEq, Debug)]
pub(crate) struct Fixture {
    /// String-valued `let` bindings, such as `let src = concat!(...)`, keyed by binding name.
    pub strings: BTreeMap<String, String>,
    /// String arguments of `assert_eq!`, in the order they appear.
    pub expected: Vec<String>,
    /// `entry(name, version)` package index entries.
    pub entries: Vec<(String, String)>,
    /// `latest(&[(name, version), ...])` resolved latest versions.
    pub latest: Vec<(String, String)>,
    /// String arrays iterated by `for` loops, such as the files a test creates.
    pub files: Vec<String>,
    /// `PathBuf::from(...)` paths, such as the files a test expects to find.
    pub paths: Vec<String>,
}

/// Returns the fixtures of every `#[test]` function in the file's `mod tests`, keyed by test name.
pub(crate) fn extract(file: &File) -> Result<BTreeMap<String, Fixture>> {
    let Some(items) = file.items.iter().find_map(|item| match item {
        Item::Mod(m) if m.ident == "tests" => m.content.as_ref().map(|(_, items)| items),
        _ => None,
    }) else {
        bail!("no `mod tests` found");
    };

    let mut fixtures = BTreeMap::new();
    for item in items {
        let Item::Fn(f) = item else { continue };
        if !f.attrs.iter().any(|a| a.path().is_ident("test")) {
            continue;
        }
        let mut fixture = Fixture::default();
        fixture.visit_block(&f.block);
        fixtures.insert(f.sig.ident.to_string(), fixture);
    }
    Ok(fixtures)
}

impl<'ast> Visit<'ast> for Fixture {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let (Pat::Ident(name), Some(init)) = (&local.pat, &local.init)
            && let Some(value) = string(&init.expr)
        {
            self.strings.insert(name.ident.to_string(), value);
        }
        visit::visit_local(self, local);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // syn leaves macro bodies as tokens; parse the expression-list ones (assert_eq!, vec!) to see inside.
        if let Ok(args) = mac.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated) {
            if mac.path.is_ident("assert_eq") {
                self.expected.extend(args.iter().filter_map(string));
            }
            for arg in &args {
                self.visit_expr(arg);
            }
        }
        visit::visit_macro(self, mac);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        let args: Vec<_> = call.args.iter().collect();
        if let Expr::Path(func) = &*call.func {
            let segments: Vec<_> = func
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            match (segments.join("::").as_str(), args.as_slice()) {
                ("entry", [name, version]) => {
                    if let (Some(name), Some(version)) = (string(name), string(version)) {
                        self.entries.push((name, version));
                    }
                }
                ("latest", [pairs]) => self.latest.extend(string_pairs(pairs)),
                ("PathBuf::from", [path]) => self.paths.extend(string(path)),
                _ => {}
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_for_loop(&mut self, for_loop: &'ast syn::ExprForLoop) {
        if let Expr::Array(array) = &*for_loop.expr {
            self.files.extend(array.elems.iter().filter_map(string));
        }
        visit::visit_expr_for_loop(self, for_loop);
    }
}

/// The value of a top-level `const NAME: &str = "..."`.
pub(crate) fn string_const(file: &File, name: &str) -> Result<String> {
    file.items
        .iter()
        .find_map(|item| match item {
            Item::Const(c) if c.ident == name => string(&c.expr),
            _ => None,
        })
        .with_context(|| format!("no string const named {name}"))
}

/// Evaluates a string literal or a `concat!` of string literals.
fn string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(s) => Some(s.value()),
            _ => None,
        },
        Expr::Macro(m) if m.mac.path.is_ident("concat") => m
            .mac
            .parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)
            .ok()?
            .iter()
            .map(string)
            .collect(),
        _ => None,
    }
}

/// Evaluates `&[("a", "b"), ...]` into string pairs.
fn string_pairs(expr: &Expr) -> Vec<(String, String)> {
    let Expr::Reference(r) = expr else {
        return Vec::new();
    };
    let Expr::Array(array) = &*r.expr else {
        return Vec::new();
    };
    array
        .elems
        .iter()
        .filter_map(|elem| match elem {
            Expr::Tuple(t) if t.elems.len() == 2 => {
                Some((string(&t.elems[0])?, string(&t.elems[1])?))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fixtures_from_test_functions_only() {
        let code = r#"
            fn helper() { let ignored = "x"; }

            #[cfg(test)]
            mod tests {
                fn not_a_test() { let ignored = "x"; }

                #[test]
                fn rewrites() {
                    let src = concat!("a\n", "b\n");
                    let index = latest(&[("cetz", "0.2.0"), ("tablex", "0.0.9")]);
                    assert_eq!(out, concat!("c\n", "d\n"));
                }

                #[test]
                fn walks() {
                    let map = latest_versions(vec![entry("cetz", "0.2.0")]);
                    for f in ["main.typ", ".git/x.typ"] {}
                    assert_eq!(found, [PathBuf::from("main.typ")]);
                }
            }
        "#;
        let fixtures = extract(&syn::parse_file(code).unwrap()).unwrap();
        assert_eq!(fixtures.keys().collect::<Vec<_>>(), ["rewrites", "walks"]);

        let rewrites = &fixtures["rewrites"];
        assert_eq!(rewrites.strings["src"], "a\nb\n");
        assert_eq!(rewrites.expected, ["c\nd\n"]);
        assert_eq!(
            rewrites.latest,
            [
                ("cetz".into(), "0.2.0".into()),
                ("tablex".into(), "0.0.9".into())
            ]
        );

        let walks = &fixtures["walks"];
        assert_eq!(walks.entries, [("cetz".into(), "0.2.0".into())]);
        assert_eq!(walks.files, ["main.typ", ".git/x.typ"]);
        assert_eq!(walks.paths, ["main.typ"]);
    }

    #[test]
    fn reads_string_consts() {
        let file = syn::parse_file(r#"const URL: &str = "https://example.com";"#).unwrap();
        assert_eq!(string_const(&file, "URL").unwrap(), "https://example.com");
        assert!(string_const(&file, "OTHER").is_err());
    }
}
