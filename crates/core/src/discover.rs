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
    /// The manifest marks the package as private to its registry (npm's `"private": true`, cargo's
    /// `publish = false`, …). Shown in the picker so the choice is informed — *not* a filter; see
    /// [`is_private`].
    pub private: bool,
}

impl GenericCandidate {
    /// A one-line label for a selection prompt.
    pub fn label(&self) -> String {
        let private = if self.private { ", private" } else { "" };
        format!(
            "{}  (v{}, {}{})  — {}",
            self.name, self.version, self.kind, private, self.manifest
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
        private: is_private(fname, &text),
    })
}

/// Whether a manifest marks its package as private to its registry. Each ecosystem spells this
/// differently and only three of the recognized manifests have a convention worth trusting —
/// npm's `"private": true`, cargo's `publish = false` (or `publish = []`), and Deno/JSR's
/// `"publish": false`. The rest report unmarked rather than guess.
///
/// This is a **label, not a filter**. "Private to a registry" is the normal state of a generic
/// build-only package — a binary shipped through a GitHub Release never goes to crates.io or npm —
/// so such a package stays selectable. The flag exists because the picker previously showed
/// nothing to distinguish it, and a private package selected there becomes a full release package.
fn is_private(manifest_file: &str, text: &str) -> bool {
    match manifest_file {
        "package.json" => bare_field(text, "private") == Some("true"),
        "Cargo.toml" => match bare_field(text, "publish") {
            Some("false") => true,
            // `publish = []` — an empty allow-list of registries is cargo's other way to say it.
            Some(v) => v.starts_with('[') && v[1..].trim_start().starts_with(']'),
            None => false,
        },
        "deno.json" | "deno.jsonc" | "jsr.json" => bare_field(text, "publish") == Some("false"),
        _ => false,
    }
}

/// The name of the directory containing `file` (fallback package name when a manifest carries no
/// `name` — e.g. a virtual Cargo workspace whose version lives in `[workspace.package]`).
fn dir_name(file: &Path) -> String {
    file.parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        // A relative manifest such as `./Cargo.toml` has a parent of `.` with no `file_name()`.
        // Canonicalize to recover the real project/repo directory name rather than the literal
        // `"package"` — otherwise a repo scanned from its own root imports as `package`.
        .or_else(|| {
            file.canonicalize()
                .ok()
                .as_deref()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "package".to_string())
}

/// Read the quoted value of `field` from `text`. Matches `"field"…:…"value"` (JSON) and
/// `field…=…"value"` (TOML/etc). Mirrors the generic adapter's `version_value_span` so discovery
/// and the adapter agree on what is parseable.
fn field_value<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    value_starts(text, field).find_map(|at| {
        let quote = text[at..].chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let open = at + quote.len_utf8();
        let close = text[open..].find(quote)?;
        Some(&text[open..open + close])
    })
}

/// Read the *unquoted* value of `field` — a bare `true`, `false`, or `[]`, which is how every
/// manifest spells a boolean or an empty list. Complements [`field_value`], which handles strings.
fn bare_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    value_starts(text, field).find_map(|at| {
        let rest = &text[at..];
        if rest.starts_with('"') || rest.starts_with('\'') {
            return None; // a string value — `field_value`'s job
        }
        let end = rest.find(['\n', ',', '}', '#']).unwrap_or(rest.len());
        let token = rest[..end].trim();
        (!token.is_empty()).then_some(token)
    })
}

/// Byte offsets where `field`'s value begins, for every place the key appears followed directly by
/// a separator. It yields more than one when the key name also occurs elsewhere in the file (in
/// prose, or as another table's key), so callers take the first occurrence whose value has the
/// shape they want rather than giving up on the first near-match.
fn value_starts<'a>(text: &'a str, field: &str) -> impl Iterator<Item = usize> + 'a {
    let field = field.to_string();
    let bytes = text.as_bytes();
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(rel) = text[from..].find(&field) {
            let at = from + rel;
            let after = at + field.len();
            from = after;
            // The bytes around the key must be a quote, whitespace, or the file edge — a cheap
            // guard against matching the field name inside some other token.
            let before_ok = at == 0 || matches!(bytes[at - 1], b'"' | b'\'' | b' ' | b'\n' | b'\t');
            let after_ok = bytes
                .get(after)
                .is_none_or(|b| matches!(b, b'"' | b'\'' | b' ' | b'\t' | b':' | b'='));
            if !before_ok || !after_ok {
                continue;
            }
            // The separator must follow the key directly (only an optional closing quote and
            // inline whitespace between), matching the generic adapter's `version_value_span` so
            // discovery and the adapter agree on what counts as a real field.
            let mut p = after;
            if matches!(bytes.get(p), Some(b'"' | b'\'')) {
                p += 1;
            }
            while matches!(bytes.get(p), Some(b' ' | b'\t')) {
                p += 1;
            }
            if !matches!(bytes.get(p), Some(b':' | b'=')) {
                continue;
            }
            let mut v = p + 1;
            while matches!(bytes.get(v), Some(b' ' | b'\t')) {
                v += 1;
            }
            return Some(v);
        }
        None
    })
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
    fn marks_private_packages_without_hiding_them() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (dir, file, text) in [
            (
                "app",
                "package.json",
                r#"{ "name": "app", "version": "1.0.0", "private": true }"#,
            ),
            (
                "tool",
                "Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"2.0.0\"\npublish = false\n",
            ),
            (
                "restricted",
                "Cargo.toml",
                "[package]\nname = \"restricted\"\nversion = \"3.0.0\"\npublish = []\n",
            ),
            (
                "lib",
                "deno.json",
                r#"{ "name": "lib", "version": "4.0.0", "publish": false }"#,
            ),
            (
                "open",
                "package.json",
                r#"{ "name": "open", "version": "5.0.0" }"#,
            ),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join(file), text).unwrap();
        }

        let found = scan_generic_candidates(root);
        let marked: Vec<(&str, bool)> =
            found.iter().map(|c| (c.name.as_str(), c.private)).collect();
        // Every candidate is still listed — private is a label, not a filter: a package that no
        // registry accepts is the normal case for a generic build-only release.
        assert_eq!(
            marked,
            vec![
                ("app", true),
                ("lib", true),
                ("open", false),
                ("restricted", true),
                ("tool", true),
            ]
        );
    }

    #[test]
    fn private_shows_in_the_picker_label() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "name": "app", "version": "1.0.0", "private": true }"#,
        )
        .unwrap();
        let found = scan_generic_candidates(tmp.path());
        assert_eq!(
            found[0].label(),
            "app  (v1.0.0, Node / npm, private)  — package.json"
        );
    }

    #[test]
    fn a_public_registry_allow_list_is_not_private() {
        // `publish = ["crates-io"]` names a registry — only an *empty* list means "nowhere".
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"1.0.0\"\npublish = [\"crates-io\"]\n",
        )
        .unwrap();
        let found = scan_generic_candidates(tmp.path());
        assert!(!found[0].private);
    }

    #[test]
    fn a_publish_table_is_not_a_privacy_marker() {
        // Deno spells file selection as `"publish": { ... }`; only the bare `false` means private.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("deno.json"),
            r#"{ "name": "lib", "version": "1.0.0", "publish": { "exclude": ["tests/"] } }"#,
        )
        .unwrap();
        let found = scan_generic_candidates(tmp.path());
        assert!(!found[0].private);
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
    fn dir_name_recovers_directory_for_a_rootless_relative_manifest() {
        // The bug: `otf-release init` run from a repo root passes root = ".", so the root manifest is
        // `./Cargo.toml` — parent `.`, no `file_name()` — and an unnamed virtual workspace collapsed
        // to the literal "package". The canonicalize fallback recovers the real directory name.
        // `cargo test` runs with cwd at this crate's dir, which has a `Cargo.toml`.
        let expected = std::env::current_dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(dir_name(Path::new("Cargo.toml")), expected);
        assert_ne!(dir_name(Path::new("Cargo.toml")), "package");
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
