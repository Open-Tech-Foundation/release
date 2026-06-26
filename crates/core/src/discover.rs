//! Repo scanning for the **generic** adapter's `init` flow.
//!
//! The generic adapter is the *custom* path — you handle a project with your own commands instead
//! of an adapter's predefined behavior. That's independent of project type: a Rust, Node, Deno, or
//! Python project can all be generic packages. So [`scan_generic_candidates`] walks the repo,
//! matches **any** recognized manifest (`Cargo.toml`, `package.json`, `deno.json`, `pyproject.toml`,
//! …), and infers a package name + current version from each — so `init` can present them for
//! selection (single project or monorepo) rather than making the user type a manifest path.
//!
//! The value extraction mirrors the generic adapter's `version_value_span` (quoted
//! `"field": "value"` / `field = "value"`), so anything surfaced here is something the adapter can
//! actually read and bump. Kept dependency-free (no `walkdir`) and independent of the adapters
//! crate to avoid a cycle.

use std::fs;
use std::path::Path;

/// A manifest the scanner recognizes: `(filename, human label)`. Every entry stores its version
/// as a quoted string under a `version` field — matching what the generic adapter can parse. The
/// list spans *all* project types, not just ones without a native adapter: generic is about
/// handling a project your own way, so a Rust or Node project is a valid generic candidate too.
const MANIFESTS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust / Cargo"),
    ("package.json", "Node / npm"),
    ("deno.json", "Deno / JSR"),
    ("deno.jsonc", "Deno / JSR"),
    ("jsr.json", "JSR"),
    ("pyproject.toml", "Python / PyPI"),
    ("composer.json", "PHP / Packagist"),
    ("gleam.toml", "Gleam / Hex"),
    ("mix.exs", "Elixir / Hex"),
];

/// Directories never worth descending into (build output, deps, VCS).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "venv",
    "__pycache__",
    "deps",
    "coverage",
];

/// How deep to recurse — enough for `packages/<group>/<pkg>/<manifest>` monorepo layouts.
const MAX_DEPTH: usize = 4;

/// A package the scanner inferred from a recognized manifest — a candidate for the generic flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericCandidate {
    /// Inferred package name (the manifest's `name`, else its directory name).
    pub name: String,
    /// Manifest path relative to the repo root (forward slashes), e.g. `packages/foo/deno.json`.
    pub manifest: String,
    /// The version field/key (always `version` for the recognized manifests).
    pub version_field: String,
    /// The current version value, for display in the picker.
    pub version: String,
    /// A human label for the ecosystem, e.g. `Deno / JSR`.
    pub kind: &'static str,
}

impl GenericCandidate {
    /// A one-line label for a selection prompt.
    pub fn label(&self) -> String {
        format!(
            "{}  (v{}, {})  — {}",
            self.name, self.version, self.kind, self.manifest
        )
    }
}

/// Scan `root` for recognized generic manifests, returning candidates sorted by path.
pub fn scan_generic_candidates(root: &Path) -> Vec<GenericCandidate> {
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out.sort_by(|a, b| a.manifest.cmp(&b.manifest));
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<GenericCandidate>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip hidden (.git, .venv, .dart_tool, …) and known-noisy directories.
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            subdirs.push(entry.path());
        } else if ft.is_file() {
            if let Some(c) = candidate_from(root, &entry.path()) {
                out.push(c);
            }
        }
    }
    for sub in subdirs {
        walk(root, &sub, depth + 1, out);
    }
}

/// Build a candidate from one file if its name matches a known manifest and it carries a version.
fn candidate_from(root: &Path, file: &Path) -> Option<GenericCandidate> {
    let fname = file.file_name()?.to_str()?;
    let (_, kind) = MANIFESTS.iter().find(|(f, _)| *f == fname)?;
    let text = fs::read_to_string(file).ok()?;
    // The version is the git-tag source — no version, no candidate.
    let version = field_value(&text, "version")?.to_string();
    let name = field_value(&text, "name")
        .map(str::to_string)
        .unwrap_or_else(|| dir_name(file));
    let manifest = file
        .strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/");
    Some(GenericCandidate {
        name,
        manifest,
        version_field: "version".to_string(),
        version,
        kind,
    })
}

/// The name of the directory containing `file` (fallback package name).
fn dir_name(file: &Path) -> String {
    file.parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "package".to_string())
}

/// Read the quoted value of `field` from `text`. Matches `"field"…:…"value"` (JSON) and
/// `field…=…"value"` (TOML/etc). Mirrors the generic adapter's `version_value_span` so discovery
/// and the adapter agree on what is parseable.
fn field_value<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(rel) = text[from..].find(field) {
        let at = from + rel;
        let before_ok =
            at == 0 || matches!(text.as_bytes()[at - 1], b'"' | b'\'' | b' ' | b'\n' | b'\t');
        let after = at + field.len();
        let after_ok = text.as_bytes().get(after).map_or(true, |b| {
            matches!(b, b'"' | b'\'' | b' ' | b'\t' | b':' | b'=')
        });
        if before_ok && after_ok {
            if let Some(sep) = text[after..].find([':', '=']) {
                let val_search = after + sep + 1;
                if let Some(q) = text[val_search..].find(['"', '\'']) {
                    let open = val_search + q;
                    let quote = text.as_bytes()[open] as char;
                    if let Some(close) = text[open + 1..].find(quote) {
                        return Some(&text[open + 1..open + 1 + close]);
                    }
                }
            }
        }
        from = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_name_and_version_from_a_single_repo() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("deno.json"),
            "{\n  \"name\": \"@me/lib\",\n  \"version\": \"1.2.3\"\n}\n",
        )
        .unwrap();
        let found = scan_generic_candidates(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "@me/lib");
        assert_eq!(found[0].version, "1.2.3");
        assert_eq!(found[0].manifest, "deno.json");
        assert_eq!(found[0].kind, "Deno / JSR");
    }

    #[test]
    fn lists_each_package_in_a_monorepo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("packages/b")).unwrap();
        fs::write(
            root.join("packages/a/deno.json"),
            "{ \"name\": \"a\", \"version\": \"0.1.0\" }",
        )
        .unwrap();
        fs::write(
            root.join("packages/b/pyproject.toml"),
            "[project]\nname = \"b\"\nversion = \"2.0.0\"\n",
        )
        .unwrap();
        let found = scan_generic_candidates(root);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(found[0].manifest, "packages/a/deno.json");
        assert_eq!(found[1].manifest, "packages/b/pyproject.toml");
    }

    #[test]
    fn detects_rust_and_node_projects_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"crate-x\"\nversion = \"0.3.1\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("web")).unwrap();
        fs::write(
            root.join("web/package.json"),
            "{ \"name\": \"web\", \"version\": \"1.0.0\" }",
        )
        .unwrap();
        let found = scan_generic_candidates(root);
        let by_name: Vec<(&str, &str)> = found.iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert!(by_name.contains(&("crate-x", "Rust / Cargo")));
        assert!(by_name.contains(&("web", "Node / npm")));
    }

    #[test]
    fn skips_cargo_workspace_inherited_version() {
        let tmp = tempfile::tempdir().unwrap();
        // A crate that inherits `version.workspace = true` has no literal version to tag.
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion.workspace = true\n",
        )
        .unwrap();
        assert!(scan_generic_candidates(tmp.path()).is_empty());
    }

    #[test]
    fn falls_back_to_directory_name_when_unnamed() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("widget")).unwrap();
        fs::write(
            tmp.path().join("widget/deno.json"),
            "{ \"version\": \"0.0.1\" }",
        )
        .unwrap();
        let found = scan_generic_candidates(tmp.path());
        assert_eq!(found[0].name, "widget");
    }

    #[test]
    fn skips_noisy_dirs_and_manifests_without_a_version() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Buried in node_modules — must not be discovered.
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::write(
            root.join("node_modules/dep/deno.json"),
            "{ \"version\": \"9.9.9\" }",
        )
        .unwrap();
        // A recognized manifest but with no version — not a tag source, so skipped.
        fs::write(root.join("deno.json"), "{ \"name\": \"x\" }").unwrap();
        assert!(scan_generic_candidates(root).is_empty());
    }
}
