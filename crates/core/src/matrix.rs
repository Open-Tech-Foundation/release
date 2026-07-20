//! `otf-release matrix` — emit a GitHub Actions matrix `include` from `release.toml`.
//!
//! The generated workflow computes its build matrix by calling this at run time, so `release.yml`
//! never carries a hand-maintained target list that can drift from `release.toml`. Each emitted
//! entry already carries the reconciled facts (triple, runner, ext, cross, stage_as) so the build
//! leg needs no further lookups — it just calls `otf-release build --target <name>/<arch>`.

use anyhow::{anyhow, bail, Result};

use crate::config::{PackageEntry, ReleaseConfig};

/// Emit the matrix JSON for `package` (or, when `None` and exactly one matrix package exists, that
/// one). Shape: `{"include":[{name,arch,triple,runner,ext,cross,vm,stage_as}, …]}`, ready to drop into
/// `strategy.matrix: ${{ fromJSON(...) }}`.
pub fn matrix_json(config: &ReleaseConfig, package: Option<&str>) -> Result<String> {
    let entry = select_package(config, package)?;
    let items: Vec<String> = entry
        .targets
        .iter()
        .map(|t| {
            format!(
                r#"{{"name":"{}","arch":"{}","triple":"{}","runner":"{}","ext":"{}","cross":{},"vm":{},"stage_as":"{}"}}"#,
                t.name,
                t.arch,
                t.triple(),
                t.runner(),
                t.ext(),
                t.is_cross(),
                t.is_vm(),
                t.stage_as()
            )
        })
        .collect();
    Ok(format!(r#"{{"include":[{}]}}"#, items.join(",")))
}

/// Resolve which matrix package to emit for, erroring clearly when the choice is ambiguous.
fn select_package<'a>(
    config: &'a ReleaseConfig,
    package: Option<&str>,
) -> Result<&'a PackageEntry> {
    let matrix: Vec<&PackageEntry> = config.packages.iter().filter(|p| p.matrix).collect();
    match package {
        Some(name) => matrix
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| anyhow!("no matrix package named `{name}` in release.toml")),
        None => match matrix.as_slice() {
            [one] => Ok(one),
            [] => bail!("no matrix packages in release.toml"),
            _ => bail!("multiple matrix packages — pass --package <name>"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Ecosystem, Mode, PackageEntry, Target};

    fn matrix_pkg(name: &str, targets: Vec<Target>) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Npm,
            mode: Mode::Publish,
            matrix: true,
            targets,
            command: "cargo build --release --target {triple}".into(),
            artifacts: "target/{triple}/release/{bin}{ext}".into(),
            bin_name: Some("otfwc".into()),
            compress: Some("brotli".into()),
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            executable: None,
            include: Vec::new(),
        }
    }

    fn config_with(packages: Vec<PackageEntry>) -> ReleaseConfig {
        ReleaseConfig {
            packages,
            ..ReleaseConfig::default()
        }
    }

    #[test]
    fn emits_reconciled_fields_per_target() {
        let cfg = config_with(vec![matrix_pkg(
            "@opentf/web-compiler",
            vec![
                Target::resolved("linux", "aarch64"),
                Target::resolved("windows", "x86_64"),
            ],
        )]);
        let json = matrix_json(&cfg, None).unwrap();
        // The Node stage dir is the load-bearing field; assert it is the process.platform-arch form.
        assert!(json.contains(r#""stage_as":"linux-arm64""#));
        assert!(json.contains(r#""triple":"aarch64-unknown-linux-gnu""#));
        assert!(json.contains(r#""runner":"ubuntu-latest""#));
        assert!(json.contains(r#""cross":true"#));
        // Windows carries the .exe extension and its own stage dir.
        assert!(json.contains(r#""stage_as":"win32-x64""#));
        assert!(json.contains(r#""ext":".exe""#));
        assert!(json.contains(r#""cross":false"#));
        assert!(json.starts_with(r#"{"include":["#));
        // Host-built targets carry vm:false; the workflow's `!matrix.vm` gates depend on the field
        // being present on every row, not just VM ones.
        assert_eq!(json.matches(r#""vm":false"#).count(), 2);
    }

    #[test]
    fn vm_targets_are_flagged_for_the_workflow() {
        let cfg = config_with(vec![matrix_pkg(
            "esrun",
            vec![
                Target::resolved("linux", "x86_64"),
                Target::resolved("freebsd", "aarch64"),
            ],
        )]);
        let json = matrix_json(&cfg, None).unwrap();
        assert!(json.contains(r#""name":"freebsd","arch":"aarch64""#));
        assert!(json.contains(r#""triple":"aarch64-unknown-freebsd""#));
        // The VM row builds in a guest, so it is not a cross build and runs on the Linux host.
        assert!(json.contains(r#""cross":false,"vm":true,"stage_as":"freebsd-arm64""#));
        assert!(json.contains(r#""cross":false,"vm":false,"stage_as":"linux-x64""#));
    }

    #[test]
    fn requires_package_when_ambiguous() {
        let cfg = config_with(vec![
            matrix_pkg("a", vec![Target::resolved("linux", "x86_64")]),
            matrix_pkg("b", vec![Target::resolved("linux", "x86_64")]),
        ]);
        assert!(matrix_json(&cfg, None).is_err());
        assert!(matrix_json(&cfg, Some("b")).is_ok());
        assert!(matrix_json(&cfg, Some("missing")).is_err());
    }
}
