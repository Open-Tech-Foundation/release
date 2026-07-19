//! `otf-release build --package <p> --target <name>/<arch>` — build one matrix target and stage
//! its binary for publish.
//!
//! Runs inside a single CI matrix leg. It:
//!   1. (for cargo builds) installs the Rust target with `rustup target add`,
//!   2. sets the cross linker env var for `cross` targets,
//!   3. runs the package's templated build command (`{triple}`/`{ext}`/`{bin}` expanded),
//!   4. copies — optionally brotli-compressing — the produced binary to
//!      `.artifacts/<package>/bin/<stage_as>/<bin><ext>[.br]`.
//!
//! That last path is the whole point: `<stage_as>` is the Node `process.platform-process.arch`
//! directory the package's install-time resolver reads, and `.artifacts/<package>` is exactly what
//! `publish` copies into the package before packing. Getting the layout right here is what stops a
//! "published, but no install can find the binary" bug.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{ReleaseConfig, Target};

/// Build one matrix target of `package` and stage its binary under `.artifacts/`.
pub fn run(config: &ReleaseConfig, root: &Path, package: &str, target_spec: &str) -> Result<()> {
    let (os, arch) = parse_target_spec(target_spec)?;
    let entry = config
        .packages
        .iter()
        .find(|p| p.matrix && p.name == package)
        .ok_or_else(|| anyhow!("no matrix package named `{package}` in release.toml"))?;
    let target = entry
        .targets
        .iter()
        .find(|t| t.name == os && t.arch == arch)
        .ok_or_else(|| anyhow!("package `{package}` has no target `{os}/{arch}`"))?;
    let bin = entry.bin_name.as_deref().ok_or_else(|| {
        anyhow!("matrix package `{package}` needs a `bin_name` to stage its binary")
    })?;

    if entry.command.contains("cargo") {
        rustup_add_target(&target.triple());
    }

    let command = target.render(&entry.command, bin);
    run_build_command(root, &command, target)?;

    let artifact = resolve_artifact(root, &target.render(&entry.artifacts, bin))?;
    let dest = staged_path(root, package, target, bin, entry.compress.as_deref());
    stage_binary(&artifact, &dest, entry.compress.as_deref())?;
    println!(
        "Staged {} -> {}",
        artifact.display(),
        dest.strip_prefix(root).unwrap_or(&dest).display()
    );
    Ok(())
}

/// Parse a `name/arch` target spec (as the workflow passes `${{ matrix.name }}/${{ matrix.arch }}`).
fn parse_target_spec(spec: &str) -> Result<(String, String)> {
    spec.split_once('/')
        .map(|(n, a)| (n.to_string(), a.to_string()))
        .ok_or_else(|| anyhow!("--target must be `name/arch` (e.g. linux/aarch64), got `{spec}`"))
}

/// The staged destination: `.artifacts/<package>/bin/<stage_as>/<bin><ext>[.br]`.
fn staged_path(
    root: &Path,
    package: &str,
    target: &Target,
    bin: &str,
    compress: Option<&str>,
) -> PathBuf {
    let mut file = format!("{bin}{}", target.ext());
    if compress.is_some() {
        file.push_str(".br");
    }
    root.join(".artifacts")
        .join(package)
        .join("bin")
        .join(target.stage_as())
        .join(file)
}

/// Best-effort `rustup target add` so cargo can cross-compile; a failure (e.g. no rustup) is left
/// for the build command itself to surface.
fn rustup_add_target(triple: &str) {
    if triple.is_empty() {
        return;
    }
    let _ = Command::new("rustup")
        .args(["target", "add", triple])
        .status();
}

/// Run the templated build command, exporting the cross linker env var for `cross` targets.
fn run_build_command(root: &Path, command: &str, target: &Target) -> Result<()> {
    let (shell, flag) = if cfg!(windows) {
        ("powershell", "-Command")
    } else {
        ("sh", "-c")
    };
    let mut cmd = Command::new(shell);
    cmd.arg(flag).arg(command).current_dir(root);
    if target.is_cross() {
        cmd.env(
            linker_env_var(&target.triple()),
            cross_linker(&target.triple()),
        );
    }
    println!("> {command}");
    let status = cmd
        .status()
        .with_context(|| format!("failed to run build command: {command}"))?;
    if !status.success() {
        bail!("build command failed with {status}: {command}");
    }
    Ok(())
}

/// `CARGO_TARGET_<TRIPLE>_LINKER` — the env var cargo reads for a target's linker.
fn linker_env_var(triple: &str) -> String {
    format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    )
}

/// The conventional GNU cross linker for a Linux triple, e.g. `aarch64-linux-gnu-gcc`.
fn cross_linker(triple: &str) -> String {
    let arch = triple.split('-').next().unwrap_or_default();
    format!("{arch}-linux-gnu-gcc")
}

/// Resolve the (possibly globbed) templated artifacts path to a single existing file.
fn resolve_artifact(root: &Path, rendered: &str) -> Result<PathBuf> {
    let joined = root.join(rendered);
    if rendered.contains('*') {
        let pattern = joined.to_string_lossy().into_owned();
        let first = glob::glob(&pattern)
            .with_context(|| format!("bad artifacts glob: {pattern}"))?
            .filter_map(Result::ok)
            .find(|p| p.is_file());
        first.ok_or_else(|| anyhow!("no build artifact matched `{rendered}`"))
    } else if joined.is_file() {
        Ok(joined)
    } else {
        bail!("build artifact `{rendered}` not found")
    }
}

/// Copy `src` to `dest`, brotli-compressing when requested. Creates parent dirs.
fn stage_binary(src: &Path, dest: &Path, compress: Option<&str>) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match compress {
        None => {
            std::fs::copy(src, dest)
                .with_context(|| format!("copying {} -> {}", src.display(), dest.display()))?;
        }
        Some("brotli") => {
            let data = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
            let out = std::fs::File::create(dest)
                .with_context(|| format!("creating {}", dest.display()))?;
            // Quality 11 (max), window 22 — what node:zlib's brotli default decompresses.
            let mut writer = brotli::CompressorWriter::new(out, 4096, 11, 22);
            writer
                .write_all(&data)
                .with_context(|| format!("compressing {}", dest.display()))?;
            writer.flush().context("flushing brotli stream")?;
        }
        Some(other) => bail!("unsupported compression `{other}` (expected `brotli`)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn linker_env_and_cross_linker_follow_cargo_convention() {
        assert_eq!(
            linker_env_var("aarch64-unknown-linux-gnu"),
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"
        );
        assert_eq!(
            cross_linker("aarch64-unknown-linux-gnu"),
            "aarch64-linux-gnu-gcc"
        );
        // musl aarch64 cross-links with the same GNU linker (Rust supplies the musl crt).
        assert_eq!(
            linker_env_var("aarch64-unknown-linux-musl"),
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"
        );
        assert_eq!(
            cross_linker("aarch64-unknown-linux-musl"),
            "aarch64-linux-gnu-gcc"
        );
    }

    #[test]
    fn staged_path_uses_node_platform_dir_and_br_suffix() {
        let root = Path::new("/repo");
        let compressed = staged_path(
            root,
            "@x/wc",
            &Target::resolved("linux", "aarch64"),
            "otfwc",
            Some("brotli"),
        );
        assert_eq!(
            compressed,
            Path::new("/repo/.artifacts/@x/wc/bin/linux-arm64/otfwc.br")
        );
        let raw = staged_path(
            root,
            "@x/wc",
            &Target::resolved("windows", "x86_64"),
            "otfwc",
            None,
        );
        assert_eq!(
            raw,
            Path::new("/repo/.artifacts/@x/wc/bin/win32-x64/otfwc.exe")
        );
    }

    #[test]
    fn stage_binary_brotli_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("bin");
        std::fs::write(&src, b"hello binary contents").unwrap();
        let dest = dir.path().join("out/linux-arm64/bin.br");
        stage_binary(&src, &dest, Some("brotli")).unwrap();
        assert!(dest.exists());

        let mut decompressed = Vec::new();
        let f = std::fs::File::open(&dest).unwrap();
        brotli::Decompressor::new(f, 4096)
            .read_to_end(&mut decompressed)
            .unwrap();
        assert_eq!(decompressed, b"hello binary contents");
    }

    #[test]
    fn stage_binary_plain_copies() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("bin");
        std::fs::write(&src, b"raw").unwrap();
        let dest = dir.path().join("out/win32-x64/bin.exe");
        stage_binary(&src, &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"raw");
    }

    #[test]
    fn resolve_artifact_handles_glob_and_direct() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("target/x/release");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("otfwc"), b"x").unwrap();
        let direct = resolve_artifact(dir.path(), "target/x/release/otfwc").unwrap();
        assert!(direct.is_file());
        let globbed = resolve_artifact(dir.path(), "target/*/release/otfwc").unwrap();
        assert!(globbed.is_file());
        assert!(resolve_artifact(dir.path(), "target/x/release/missing").is_err());
    }

    #[test]
    fn parse_target_spec_requires_slash() {
        assert_eq!(
            parse_target_spec("linux/aarch64").unwrap(),
            ("linux".to_string(), "aarch64".to_string())
        );
        assert!(parse_target_spec("linux-aarch64").is_err());
    }
}
