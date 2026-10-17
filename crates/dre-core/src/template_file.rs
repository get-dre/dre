//! Where a template named in YAML is read from: an xlsx or html/pdf `template.file`, and the
//! `{% include %}`/`{% extends %}` of a document template.
//!
//! - A name containing `/` is a path relative to the project root, taken exactly.
//! - A bare file name is looked for in the starting folder (the report YAML's, or the including
//!   template's), then in each parent folder up to the project root: the nearest wins.
//! - Failing that, anywhere in the project, where it must be unique.
//!
//! Never outside the project, and never a URL. The `document` plugin resolves includes with this
//! same function, from the project root core sends it.

use std::path::{Component, Path, PathBuf};

use crate::plugins::DEPS_DIR;
use crate::project::{LOGS_DIR, TARGET_DIR};

/// Resolve `name`, looked up from `from` (a folder inside `root`). The error is a sentence for
/// the person: the name, and where it was looked for or the files it matched.
pub fn resolve(root: &Path, from: &Path, name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty() {
        return Err("the template file name is empty".into());
    }
    if name.contains("://") {
        return Err(format!(
            "`{name}` is a URL; templates are read from files in the project only"
        ));
    }
    if name.contains('/') || name.contains('\\') {
        let rel = Path::new(name);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(format!(
                "`{name}` must be a path inside the project, relative to its root (no `..`, not absolute)"
            ));
        }
        let path = root.join(rel);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "template file `{name}` doesn't exist (a path with `/` is relative to the project root)"
            ))
        };
    }
    let from = if from.starts_with(root) { from } else { root };
    let mut dir = Some(from);
    while let Some(d) = dir {
        let candidate = d.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    let found = anywhere(root, name);
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "template file `{name}` doesn't exist (looked next to the YAML, in each parent folder up to the project root, then anywhere in the project)"
        )),
        many => Err(format!(
            "template file `{name}` matches {} files ({}); name one by its path from the project root",
            many.len(),
            many.iter()
                .map(|p| format!("`{}`", shown(root, p)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Every file called `name` in the project, skipping hidden folders, `target/`, `dre_deps/`
/// and `logs/`, sorted by path.
fn anywhere(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            e.depth() == 0
                || !(n.starts_with('.')
                    || (e.depth() == 1
                        && e.file_type().is_dir()
                        && [TARGET_DIR, DEPS_DIR, LOGS_DIR].contains(&n.as_ref())))
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.file_name() == name)
        .map(|e| e.into_path())
        .collect();
    out.sort();
    out
}

fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, f).unwrap();
        }
        dir
    }

    fn found(root: &Path, from: &str, name: &str) -> Result<String, String> {
        resolve(root, &root.join(from), name).map(|p| shown(root, &p))
    }

    /// (files, folder looked from, name, expected file or error fragment).
    type Case<'a> = (&'a [&'a str], &'a str, &'a str, Result<&'a str, &'a str>);

    /// The shared table.
    #[test]
    fn lookup_table() {
        let cases: &[Case<'_>] = &[
            // Nearest wins: next to the YAML before a parent's.
            (
                &["reports/a/s.html", "reports/s.html"],
                "reports/a",
                "s.html",
                Ok("reports/a/s.html"),
            ),
            // Walk up to a parent folder.
            (
                &["reports/s.html", "templates/s.html"],
                "reports/a/b",
                "s.html",
                Ok("reports/s.html"),
            ),
            // Up to the root itself.
            (
                &["s.html", "templates/s.html"],
                "reports/a",
                "s.html",
                Ok("s.html"),
            ),
            // Anywhere in the project when unique.
            (
                &["templates/s.html"],
                "reports/a",
                "s.html",
                Ok("templates/s.html"),
            ),
            // Ambiguous: two matches outside the walk.
            (
                &["templates/s.html", "shared/s.html"],
                "reports/a",
                "s.html",
                Err("matches 2 files"),
            ),
            // A path is taken exactly, from the root.
            (
                &["templates/s.html", "reports/templates/s.html"],
                "reports",
                "templates/s.html",
                Ok("templates/s.html"),
            ),
            (&["s.html"], "reports", "templates/s.html", Err("doesn't exist")),
            // Never outside the project.
            (&["s.html"], "reports", "../s.html", Err("inside the project")),
            (&["s.html"], "reports", "/etc/passwd", Err("inside the project")),
            (&["s.html"], "reports", "https://example.com/s.html", Err("URL")),
            // Generated folders aren't searched.
            (
                &["target/run/s.html", "dre_deps/x/s.html", ".git/s.html"],
                "reports",
                "s.html",
                Err("doesn't exist"),
            ),
            (&[], "reports", "s.html", Err("doesn't exist")),
        ];
        for (files, from, name, want) in cases {
            let dir = project(files);
            let got = found(dir.path(), from, name);
            match (want, &got) {
                (Ok(w), Ok(g)) => assert_eq!(g, w, "{files:?} from {from}: {name}"),
                (Err(w), Err(g)) => assert!(g.contains(w), "{files:?} from {from}: {name}: {g}"),
                _ => panic!("{files:?} from {from}: {name}: got {got:?}, want {want:?}"),
            }
        }
    }
}
