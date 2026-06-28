//! The generic adapter — for a registry the tool doesn't natively support yet (e.g. Deno's
//! **JSR**). Rather than hardcoding ecosystem knowledge, the user supplies the pieces in
//! `release.toml`, and the tool still provides versioning + changelog + release PR + a publish
//! workflow scaffold.
//!
//! - **Version** lives in a manifest the user names (`manifest`, e.g. `deno.json`) under a field
//!   (`version_field`, default `version`). The adapter reads it (the git-tag source) and bumps it
//!   in place with a targeted text replace, preserving the file's formatting. Works for any
//!   `"version": "x.y.z"` / `version = "x.y.z"` style manifest (JSON, TOML, …).
//! - **Publish** is an optional shell command (`publish`, e.g. `npx jsr publish`). When present
//!   the package publishes through `otf-release publish` (which then tags + makes the GitHub
//!   Release); when absent the package is build-only.
//! - **No dependency graph / lockfile / ranges.**

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use toml_edit::{value, DocumentMut, Item};

use otf_release_core::adapter::{Adapter, Bump, DepKind, Pkg};

/// One generic project, as configured in `release.toml`.
#[derive(Debug, Clone)]
pub struct GenericPkg {
    pub name: String,
    /// Manifest file (relative to the repo root) holding the version.
    pub manifest: PathBuf,
    /// The version field/key inside the manifest.
    pub version_field: String,
    /// Optional shell command that publishes to the registry.
    pub publish: Option<String>,
}

/// A registry-less / config-driven adapter for unsupported ecosystems.
pub struct GenericAdapter {
    root: PathBuf,
    packages: Vec<GenericPkg>,
}

impl GenericAdapter {
    pub fn new(root: impl Into<PathBuf>, packages: Vec<GenericPkg>) -> Self {
        Self {
            root: root.into(),
            packages,
        }
    }

    fn config_for(&self, name: &str) -> Result<&GenericPkg> {
        self.packages
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| anyhow::anyhow!("no generic package named {name} in release.toml"))
    }

    fn manifest_path(&self, pkg: &GenericPkg) -> PathBuf {
        self.root.join(&pkg.manifest)
    }
}

/// Find the version value for `field` in manifest `text`. Matches `"field"…:…"value"` (JSON) and
/// `field…=…"value"` (TOML); returns the byte range of the value so it can be read or replaced.
fn version_value_span(text: &str, field: &str) -> Option<std::ops::Range<usize>> {
    // Locate the key (quoted or bare), then the first quoted string after the `:`/`=`.
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(field) {
        let key_at = search_from + rel;
        // The bytes immediately around the key should be a quote, whitespace, or start — a cheap
        // guard against matching the field name inside some other token.
        let before_ok = key_at == 0
            || matches!(
                text.as_bytes()[key_at - 1],
                b'"' | b'\'' | b' ' | b'\n' | b'\t'
            );
        let after = key_at + field.len();
        let after_ok = text.as_bytes().get(after).map_or(true, |b| {
            matches!(b, b'"' | b'\'' | b' ' | b'\t' | b':' | b'=')
        });
        if before_ok && after_ok {
            // Find the separator, then the opening quote of the value.
            if let Some(sep_rel) = text[after..].find([':', '=']) {
                let val_search = after + sep_rel + 1;
                if let Some(q_rel) = text[val_search..].find(['"', '\'']) {
                    let open = val_search + q_rel;
                    let quote = text.as_bytes()[open];
                    if let Some(close_rel) = text[open + 1..].find(quote as char) {
                        let start = open + 1;
                        let end = start + close_rel;
                        return Some(start..end);
                    }
                }
            }
        }
        search_from = after;
    }
    None
}

fn field_path(field: &str) -> Vec<&str> {
    field
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn read_manifest_version(path: &Path, text: &str, field: &str) -> Result<String> {
    if field_path(field).is_empty() {
        bail!("{}: empty version field", path.display());
    }
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => read_json_version(path, text, field),
        Some("toml") => read_toml_version(path, text, field),
        _ => read_text_version(path, text, field),
    }
}

fn write_manifest_version(path: &Path, text: &str, field: &str, new: &str) -> Result<String> {
    if field_path(field).is_empty() {
        bail!("{}: empty version field", path.display());
    }
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => write_json_version(path, text, field, new),
        Some("toml") => write_toml_version(path, text, field, new),
        _ => write_text_version(path, text, field, new),
    }
}

fn read_json_version(path: &Path, text: &str, field: &str) -> Result<String> {
    let json: Value =
        serde_json::from_str(text).with_context(|| format!("parsing JSON {}", path.display()))?;
    let value = field_path(field)
        .iter()
        .try_fold(&json, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}: could not find string JSON field `{}`",
                path.display(),
                field
            )
        })?;
    Ok(value.to_string())
}

fn write_json_version(path: &Path, text: &str, field: &str, new: &str) -> Result<String> {
    let path_parts = field_path(field);
    if path_parts.len() == 1 {
        return write_text_version(path, text, field, new);
    }

    let mut json: Value =
        serde_json::from_str(text).with_context(|| format!("parsing JSON {}", path.display()))?;
    let mut current = &mut json;
    for key in &path_parts[..path_parts.len() - 1] {
        current = current.get_mut(*key).ok_or_else(|| {
            anyhow::anyhow!("{}: could not find JSON object `{}`", path.display(), key)
        })?;
    }
    let leaf = path_parts.last().unwrap();
    let Some(slot) = current.get_mut(*leaf) else {
        bail!("{}: no JSON field `{}` to bump", path.display(), field);
    };
    if !slot.is_string() {
        bail!("{}: JSON field `{}` is not a string", path.display(), field);
    }
    *slot = Value::String(new.to_string());
    serde_json::to_string_pretty(&json)
        .map(|s| format!("{s}\n"))
        .with_context(|| format!("serializing JSON {}", path.display()))
}

fn read_toml_version(path: &Path, text: &str, field: &str) -> Result<String> {
    let doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing TOML {}", path.display()))?;
    let value = field_path(field)
        .iter()
        .try_fold(doc.as_item(), |current, key| current.get(*key))
        .and_then(Item::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}: could not find string TOML field `{}`",
                path.display(),
                field
            )
        })?;
    Ok(value.to_string())
}

fn write_toml_version(path: &Path, text: &str, field: &str, new: &str) -> Result<String> {
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing TOML {}", path.display()))?;
    let path_parts = field_path(field);
    let mut current = doc.as_item_mut();
    for key in &path_parts[..path_parts.len() - 1] {
        current = current.get_mut(*key).ok_or_else(|| {
            anyhow::anyhow!("{}: could not find TOML table `{}`", path.display(), key)
        })?;
    }
    let leaf = path_parts
        .last()
        .ok_or_else(|| anyhow::anyhow!("{}: empty version field", path.display()))?;
    let Some(slot) = current.get_mut(*leaf) else {
        bail!("{}: no TOML field `{}` to bump", path.display(), field);
    };
    if !slot.is_str() {
        bail!("{}: TOML field `{}` is not a string", path.display(), field);
    }
    *slot = value(new);
    Ok(doc.to_string())
}

fn read_text_version(path: &Path, text: &str, field: &str) -> Result<String> {
    let span = version_value_span(text, field).ok_or_else(|| {
        anyhow::anyhow!(
            "{}: could not find a `{}` version field",
            path.display(),
            field
        )
    })?;
    Ok(text[span].to_string())
}

fn write_text_version(path: &Path, text: &str, field: &str, new: &str) -> Result<String> {
    let span = version_value_span(text, field)
        .ok_or_else(|| anyhow::anyhow!("{}: no `{}` field to bump", path.display(), field))?;
    let mut updated = String::with_capacity(text.len());
    updated.push_str(&text[..span.start]);
    updated.push_str(new);
    updated.push_str(&text[span.end..]);
    Ok(updated)
}

impl Adapter for GenericAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        let mut out = Vec::new();
        for cfg in &self.packages {
            let path = self.manifest_path(cfg);
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading generic manifest {}", path.display()))?;
            let version = read_manifest_version(&path, &text, &cfg.version_field)?;
            out.push(Pkg {
                name: cfg.name.clone(),
                version,
                manifest_path: path,
                changelog_path: self.root.join("CHANGELOG.md"),
                publishable: true,
                internal_deps: vec![],
            });
        }
        Ok(out)
    }

    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
        let cfg = self.config_for(&pkg.name)?;
        let path = self.manifest_path(cfg);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let updated = write_manifest_version(&path, &text, &cfg.version_field, new)?;
        std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))
    }

    // No manifests-beyond-version, no internal dependencies → these are no-ops.
    fn update_dep_range(&self, _pkg: &Pkg, _dep: &str, _new_dep_version: &str) -> Result<()> {
        Ok(())
    }

    fn format_range(&self, version: &str) -> String {
        version.to_string()
    }

    fn resolve_workspace_links(&self, _pkg: &Pkg) -> Result<()> {
        Ok(())
    }

    fn update_lockfile(&self, _root: &Path) -> Result<()> {
        Ok(())
    }

    fn dependent_bump(&self, _dep_bump: Bump, _kind: &DepKind) -> Bump {
        Bump::Patch
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        // No generic registry API exists, so use the tag created after a successful publish as
        // the resumability marker.
        let tag = format!("{}@{}", pkg.name, version);
        let out = Command::new("git")
            .args(["tag", "--list", &tag])
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("checking for release tag: {tag}"))?;
        if !out.status.success() {
            bail!(
                "`git tag --list {tag}` failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
    }

    fn publish(&self, pkg: &Pkg, _staged_assets: Option<&Path>) -> Result<()> {
        let cfg = self.config_for(&pkg.name)?;
        let Some(command) = &cfg.publish else {
            bail!(
                "generic package `{}` has no `publish` command — it is build-only and ships via \
                 the workflow's GitHub Release, not `otf-release publish`.",
                pkg.name
            );
        };
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .status()
            .with_context(|| format!("running publish command: {command}"))?;
        if !status.success() {
            bail!("publish command failed ({status}): {command}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, manifest: &str, publish: Option<&str>) -> GenericPkg {
        GenericPkg {
            name: name.into(),
            manifest: manifest.into(),
            version_field: "version".into(),
            publish: publish.map(|s| s.into()),
        }
    }

    fn pkg_with_field(name: &str, manifest: &str, field: &str) -> GenericPkg {
        GenericPkg {
            name: name.into(),
            manifest: manifest.into(),
            version_field: field.into(),
            publish: None,
        }
    }

    #[test]
    fn reads_version_from_json_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("deno.json"),
            "{\n  \"name\": \"@me/lib\",\n  \"version\": \"1.2.3\"\n}\n",
        )
        .unwrap();
        let a = GenericAdapter::new(
            tmp.path(),
            vec![pkg("lib", "deno.json", Some("npx jsr publish"))],
        );
        let pkgs = a.discover_packages().unwrap();
        assert_eq!(pkgs[0].version, "1.2.3");
        assert!(pkgs[0].publishable);
    }

    #[test]
    fn bumps_version_in_place_preserving_formatting() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "{\n  \"name\": \"@me/lib\",\n  \"version\": \"1.2.3\"\n}\n";
        std::fs::write(tmp.path().join("deno.json"), original).unwrap();
        let a = GenericAdapter::new(tmp.path(), vec![pkg("lib", "deno.json", None)]);
        let p = a.discover_packages().unwrap().pop().unwrap();
        a.write_version(&p, "1.3.0").unwrap();
        let after = std::fs::read_to_string(tmp.path().join("deno.json")).unwrap();
        assert_eq!(
            after,
            "{\n  \"name\": \"@me/lib\",\n  \"version\": \"1.3.0\"\n}\n"
        );
    }

    #[test]
    fn works_for_toml_style_too() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Project.toml"),
            "name = \"x\"\nversion = \"0.4.0\"\n",
        )
        .unwrap();
        let a = GenericAdapter::new(tmp.path(), vec![pkg("x", "Project.toml", None)]);
        assert_eq!(a.discover_packages().unwrap()[0].version, "0.4.0");
    }

    #[test]
    fn reads_and_writes_nested_json_version_field() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("manifest.json"),
            "{\n  \"pkg\": {\n    \"version\": \"1.2.3\"\n  }\n}\n",
        )
        .unwrap();
        let a = GenericAdapter::new(
            tmp.path(),
            vec![pkg_with_field("x", "manifest.json", "pkg.version")],
        );
        let p = a.discover_packages().unwrap().pop().unwrap();
        assert_eq!(p.version, "1.2.3");

        a.write_version(&p, "1.2.4").unwrap();
        let after = std::fs::read_to_string(tmp.path().join("manifest.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(json["pkg"]["version"], "1.2.4");
    }

    #[test]
    fn reads_and_writes_nested_toml_version_field() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Project.toml"),
            "[pkg]\nname = \"x\"\nversion = \"0.4.0\"\n",
        )
        .unwrap();
        let a = GenericAdapter::new(
            tmp.path(),
            vec![pkg_with_field("x", "Project.toml", "pkg.version")],
        );
        let p = a.discover_packages().unwrap().pop().unwrap();
        assert_eq!(p.version, "0.4.0");

        a.write_version(&p, "0.5.0").unwrap();
        let after = std::fs::read_to_string(tmp.path().join("Project.toml")).unwrap();
        assert!(after.contains("version = \"0.5.0\""), "got: {after}");
    }

    #[test]
    fn publish_without_command_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("deno.json"), "{\"version\":\"1.0.0\"}").unwrap();
        let a = GenericAdapter::new(tmp.path(), vec![pkg("lib", "deno.json", None)]);
        let p = a.discover_packages().unwrap().pop().unwrap();
        assert!(a.publish(&p, None).is_err());
    }

    #[test]
    fn is_published_uses_existing_release_tag() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("deno.json"), "{\"version\":\"1.0.0\"}").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(tmp.path())
            .status()
            .unwrap();

        let a = GenericAdapter::new(tmp.path(), vec![pkg("lib", "deno.json", Some("true"))]);
        let p = a.discover_packages().unwrap().pop().unwrap();
        assert!(!a.is_published(&p, "1.0.0").unwrap());

        std::process::Command::new("git")
            .args(["tag", "lib@1.0.0"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(a.is_published(&p, "1.0.0").unwrap());
    }
}
