//! `release.toml` — the persisted, committed source of truth.
//!
//! `init` writes it (which ecosystems are enabled, and the per-package build steps); every
//! other command reads it instead of taking an `--adapter` flag. The file is hand-editable —
//! it is plain TOML with a stable, documented shape, not a tool-managed blob.
//!
//! ```toml
//! adapters = ["npm", "crates.io"]
//!
//! [[package]]
//! name      = "web-compiler"
//! adapter   = "crates.io"
//! mode      = "build-only"          # artifacts -> GitHub Release, no registry push
//! matrix    = true
//! targets   = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
//! command   = "cargo build --release -p otfw_cli"
//! artifacts = "target/*/release/otfwc*"
//!
//! [[package]]
//! name      = "docs-site"
//! adapter   = "npm"
//! mode      = "publish"             # build, then publish to the registry
//! command   = "npm run build"
//! artifacts = "dist/**"
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The committed config file name, at the workspace root.
pub const CONFIG_FILE: &str = "release.toml";

/// An enabled ecosystem. Serialized by its registry name (`npm`, `crates.io`) or `generic`.
///
/// `Generic` is for registries the tool doesn't natively support (e.g. Deno's JSR): it versions a
/// project via a named manifest field and publishes through a user-supplied command. See
/// [`otf_release_adapters::generic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ecosystem {
    #[serde(rename = "npm")]
    Npm,
    #[serde(rename = "crates.io")]
    Cargo,
    #[serde(rename = "jsr")]
    Jsr,
    #[serde(rename = "generic")]
    Generic,
}

impl Ecosystem {
    /// All ecosystems offered by `init`, in menu order.
    pub const ALL: [Ecosystem; 4] = [
        Ecosystem::Npm,
        Ecosystem::Cargo,
        Ecosystem::Jsr,
        Ecosystem::Generic,
    ];

    /// The human/registry label shown in prompts and written to the file.
    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
            Ecosystem::Cargo => "crates.io",
            Ecosystem::Jsr => "jsr",
            Ecosystem::Generic => "generic (any registry, via your own commands)",
        }
    }
}

/// The default version field/key for a generic manifest.
pub const DEFAULT_VERSION_FIELD: &str = "version";

/// The default git tag format for releases.
pub const DEFAULT_TAG_FORMAT: &str = "v{version}";

/// The default branch a release is cut from and returned to.
pub const DEFAULT_BRANCH: &str = "main";

/// Common git tag formats shown by interactive prompts before falling back to custom input.
pub const COMMON_TAG_FORMATS: &[&str] = &[
    "v{version}",
    "{version}",
    "{name}@{version}",
    "{name}@v{version}",
];

/// How generated GitHub Releases should get their body text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubReleaseNotes {
    /// Let GitHub generate the body from merged PRs and commits.
    #[default]
    AutoGenerate,
    /// Copy the dated section for the released version from `CHANGELOG.md`.
    CuratedChangelog,
    /// Build a commit-subject list from the previous matching configured tag.
    SemanticCommits,
}

/// What `publish`/CI does with a package after its build step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Build, then publish to the ecosystem's registry (`otf-release publish`).
    #[serde(rename = "publish")]
    Publish,
    /// Build only — stage the artifacts and attach them to a GitHub Release. No registry push.
    #[serde(rename = "build-only")]
    BuildOnly,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Publish => "publish",
            Mode::BuildOnly => "build-only",
        }
    }
}

/// How `github-release` packages each staged binary before attaching it to the release. Omitting it
/// (the default) attaches the raw, OS/arch-renamed binary — the historical behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveFormat {
    /// `.tar.gz` for every target.
    #[serde(rename = "tar.gz")]
    TarGz,
    /// `.zip` for every target.
    #[serde(rename = "zip")]
    Zip,
    /// `.zip` for Windows targets, `.tar.gz` for everything else — the convention the old
    /// hand-written release scripts used.
    #[serde(rename = "auto")]
    Auto,
}

impl ArchiveFormat {
    /// The concrete extension for a target whose OS is `os` (as named in a stage dir, e.g.
    /// `windows`/`win32`, `linux`, `macos`/`darwin`).
    pub fn extension_for(self, os: &str) -> &'static str {
        match self {
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Auto => {
                if os == "windows" || os == "win32" {
                    "zip"
                } else {
                    "tar.gz"
                }
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Auto => "auto",
        }
    }
}

/// A build target, reconciling the three naming systems that describe one physical binary. The
/// same artifact is known by a **Rust target triple** (to cargo), a **CI runner OS** (to GitHub
/// Actions), and a **`process.platform-process.arch` directory** (to the Node `extract.js`
/// resolver). The tool is the only place that sees all three, so a `Target` carries all three —
/// keeping them in sync is what prevents a "published, but no install can find the binary" bug.
///
/// `name`/`arch` are always present; the rest default to empty/false and fall back to the built-in
/// [`TARGET_REGISTRY`] via the accessor methods, so a hand-written `release.toml` can list just
/// `name`/`arch` while `init` writes every field explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Generic OS name (e.g. "linux", "macos", "windows").
    pub name: String,
    /// Generic architecture (e.g. "x86_64", "aarch64", "x86").
    pub arch: String,
    /// Rust target triple, e.g. `aarch64-unknown-linux-gnu`. Empty ⇒ look up by (name, arch).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub triple: String,
    /// GitHub-hosted runner that builds this target, e.g. `ubuntu-latest`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    /// The staged directory inside the package. **MUST** equal Node's
    /// `process.platform`-`process.arch` (e.g. `linux-arm64`, `darwin-x64`, `win32-x64`) so the
    /// package's install-time resolver finds the binary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage_as: String,
    /// Executable extension for this target (`""` or `.exe`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ext: String,
    /// Whether this target needs cross-compile prep (a non-host linker) on its runner.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cross: bool,
    /// Whether this target builds *natively inside a VM* on its runner (a `vmactions/<name>-vm`
    /// step) instead of cross-compiling on the host. Set for OSes GitHub hosts no runner for.
    #[serde(default, skip_serializing_if = "is_false")]
    pub vm: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A built-in fact row reconciling a `(name, arch)` pair to its triple, runner, Node stage dir,
/// exe extension, and whether cross-compile prep is required.
pub struct TargetInfo {
    pub label: &'static str,
    pub name: &'static str,
    pub arch: &'static str,
    pub triple: &'static str,
    pub runner: &'static str,
    pub stage_as: &'static str,
    pub ext: &'static str,
    pub cross: bool,
    /// Whether this target builds natively inside a VM on its runner. See [`Target::vm`].
    pub vm: bool,
    /// Whether `init` selects this target by default. Only the widely-supported platforms (the set
    /// an npm package's `extract.js` resolver typically handles) are on by default; niche targets
    /// (`win32-arm64`, 32-bit) stay in the registry for explicit opt-in.
    pub default_on: bool,
}

/// The single source of truth mapping `(name, arch)` to the three naming systems. `stage_as` is the
/// Node `process.platform`-`process.arch` directory the package resolver reads — getting it wrong
/// is the one mistake that publishes a working-looking package no install can use.
#[rustfmt::skip]
pub const TARGET_REGISTRY: &[TargetInfo] = &[
    TargetInfo { label: "Linux x64",          name: "linux",   arch: "x86_64",  triple: "x86_64-unknown-linux-gnu",  runner: "ubuntu-latest",  stage_as: "linux-x64",   ext: "",     cross: false, vm: false, default_on: true },
    TargetInfo { label: "Linux ARM64",        name: "linux",   arch: "aarch64", triple: "aarch64-unknown-linux-gnu", runner: "ubuntu-latest",  stage_as: "linux-arm64", ext: "",     cross: true,  vm: false, default_on: true },
    TargetInfo { label: "Linux x86 (32-bit)", name: "linux",   arch: "x86",     triple: "i686-unknown-linux-gnu",    runner: "ubuntu-latest",  stage_as: "linux-ia32",  ext: "",     cross: true,  vm: false, default_on: false },
    // musl (statically linked, portable across distros). Keyed under a distinct `linux-musl` name so
    // it doesn't collide with the glibc `(linux, <arch>)` rows; a separate `stage_as` keeps its assets
    // distinct (e.g. `esrun-linux-musl-x86-64`). x86_64 links self-contained via `rustup target add`;
    // aarch64 cross-links with the GNU linker like the glibc ARM64 target. Off by default (opt-in).
    TargetInfo { label: "Linux x64 (musl, static)",   name: "linux-musl", arch: "x86_64",  triple: "x86_64-unknown-linux-musl",  runner: "ubuntu-latest", stage_as: "linux-musl-x64",   ext: "", cross: false, vm: false, default_on: false },
    TargetInfo { label: "Linux ARM64 (musl, static)", name: "linux-musl", arch: "aarch64", triple: "aarch64-unknown-linux-musl", runner: "ubuntu-latest", stage_as: "linux-musl-arm64", ext: "", cross: true,  vm: false, default_on: false },
    TargetInfo { label: "macOS ARM64",        name: "macos",   arch: "aarch64", triple: "aarch64-apple-darwin",      runner: "macos-latest",   stage_as: "darwin-arm64",ext: "",     cross: false, vm: false, default_on: true },
    TargetInfo { label: "macOS x64",          name: "macos",   arch: "x86_64",  triple: "x86_64-apple-darwin",       runner: "macos-latest",   stage_as: "darwin-x64",  ext: "",     cross: false, vm: false, default_on: true },
    TargetInfo { label: "Windows x64",        name: "windows", arch: "x86_64",  triple: "x86_64-pc-windows-msvc",    runner: "windows-latest", stage_as: "win32-x64",   ext: ".exe", cross: false, vm: false, default_on: true },
    // win32-arm64 is rarely in a package's resolver SUPPORTED set and cross-links arm64 on an x64
    // Windows runner; offered but off by default.
    TargetInfo { label: "Windows ARM64",      name: "windows", arch: "aarch64", triple: "aarch64-pc-windows-msvc",   runner: "windows-latest", stage_as: "win32-arm64", ext: ".exe", cross: false, vm: false, default_on: false },
    TargetInfo { label: "Windows x86 (32-bit)", name: "windows", arch: "x86",   triple: "i686-pc-windows-msvc",      runner: "windows-latest", stage_as: "win32-ia32",  ext: ".exe", cross: false, vm: false, default_on: false },
    // FreeBSD. GitHub hosts no FreeBSD runner, and cross-compiling from Linux does not work off the
    // shelf: rustc emits objects fine, but the link step needs FreeBSD base libs (-lexecinfo, -lkvm,
    // -lprocstat, …) that Rust does not ship, and aarch64-unknown-freebsd is tier 3 with no prebuilt
    // std at all. So these build *natively inside a VM* (`vm: true`) on the Linux runner instead —
    // which also makes aarch64 the VM's host target, sidestepping the tier-3 problem entirely.
    // `cross` stays false: the GNU/Linux cross prep is the wrong toolchain here.
    // Caveat: only x86_64 is hardware-accelerated; aarch64 is fully emulated and much slower.
    TargetInfo { label: "FreeBSD x64",   name: "freebsd", arch: "x86_64",  triple: "x86_64-unknown-freebsd",  runner: "ubuntu-latest", stage_as: "freebsd-x64",   ext: "", cross: false, vm: true, default_on: false },
    TargetInfo { label: "FreeBSD ARM64 (emulated, slow)", name: "freebsd", arch: "aarch64", triple: "aarch64-unknown-freebsd", runner: "ubuntu-latest", stage_as: "freebsd-arm64", ext: "", cross: false, vm: true, default_on: false },
];

/// Look up the built-in facts for a `(name, arch)` pair.
pub fn lookup_target(name: &str, arch: &str) -> Option<&'static TargetInfo> {
    TARGET_REGISTRY
        .iter()
        .find(|t| t.name == name && t.arch == arch)
}

impl Target {
    fn info(&self) -> Option<&'static TargetInfo> {
        lookup_target(&self.name, &self.arch)
    }

    /// The Rust target triple — the explicit field if set, else the registry value.
    pub fn triple(&self) -> String {
        non_empty(&self.triple).unwrap_or_else(|| {
            self.info()
                .map(|i| i.triple.to_string())
                .unwrap_or_default()
        })
    }

    /// The GitHub runner OS — the explicit field if set, else the registry value.
    pub fn runner(&self) -> String {
        non_empty(&self.runner).unwrap_or_else(|| {
            self.info()
                .map(|i| i.runner.to_string())
                .unwrap_or_default()
        })
    }

    /// The Node `process.platform-process.arch` stage dir — explicit field if set, else registry.
    pub fn stage_as(&self) -> String {
        non_empty(&self.stage_as).unwrap_or_else(|| {
            self.info()
                .map(|i| i.stage_as.to_string())
                .unwrap_or_default()
        })
    }

    /// The executable extension — explicit field if set, else the registry value.
    pub fn ext(&self) -> String {
        non_empty(&self.ext)
            .unwrap_or_else(|| self.info().map(|i| i.ext.to_string()).unwrap_or_default())
    }

    /// Whether cross-compile prep is needed — true if explicitly set, else the registry value.
    pub fn is_cross(&self) -> bool {
        self.cross || self.info().map(|i| i.cross).unwrap_or(false)
    }

    /// Whether this target builds natively inside a VM — true if explicitly set, else the registry
    /// value. VM targets skip host toolchain setup and cross prep: the build runs in the guest.
    pub fn is_vm(&self) -> bool {
        self.vm || self.info().map(|i| i.vm).unwrap_or(false)
    }

    /// Expand the per-target placeholders in a command/artifacts template: `{triple}`, `{ext}`,
    /// `{stage_as}`, `{bin}`, `{arch}`, `{name}` (the OS name). `bin` is the package's binary name.
    pub fn render(&self, template: &str, bin: &str) -> String {
        template
            .replace("{triple}", &self.triple())
            .replace("{stage_as}", &self.stage_as())
            .replace("{ext}", &self.ext())
            .replace("{arch}", &self.arch)
            .replace("{name}", &self.name)
            .replace("{bin}", bin)
    }

    /// Build a fully-populated `Target` for a `(name, arch)` pair from the registry, so `init`
    /// writes every reconciling field into `release.toml` rather than leaving them implicit.
    pub fn resolved(name: &str, arch: &str) -> Self {
        match lookup_target(name, arch) {
            Some(i) => Self {
                name: i.name.to_string(),
                arch: i.arch.to_string(),
                triple: i.triple.to_string(),
                runner: i.runner.to_string(),
                stage_as: i.stage_as.to_string(),
                ext: i.ext.to_string(),
                cross: i.cross,
                vm: i.vm,
            },
            None => Self {
                name: name.to_string(),
                arch: arch.to_string(),
                ..Self::default()
            },
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// A package that needs a build step before it is published or released.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageEntry {
    /// The package name (as the adapter discovers it, or the generic project name).
    pub name: String,
    /// Which enabled ecosystem this package belongs to.
    pub adapter: Ecosystem,
    /// Publish to a registry, or build-only (artifacts -> GitHub Release).
    pub mode: Mode,
    /// Build across a target matrix (multiple platforms).
    #[serde(default)]
    pub matrix: bool,
    /// Cross-compile targets (only when `matrix`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
    /// The build command run in CI (may be empty for a publish-only generic package).
    #[serde(default)]
    pub command: String,
    /// A glob of artifacts to stage for publish / attach to the release (may be empty). For matrix
    /// builds it is templated per target with `{triple}`, `{ext}`, `{stage_as}`, `{bin}`.
    #[serde(default)]
    pub artifacts: String,
    /// The compiled binary's base name (no extension), used to template `{bin}` and to name the
    /// staged file `bin/{stage_as}/{bin}{ext}`. Matrix builds only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_name: Option<String>,
    /// Compression applied to each staged binary, e.g. `brotli` (writes `…{ext}.br`). The package's
    /// install-time resolver decompresses it. Matrix builds only; `None` stages the raw binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<String>,

    // --- manifest fields (generic uses these to version; npm may use `manifest` for workflow reads) ---
    /// Manifest file holding the version. Required for a generic package; for npm packages `init`
    /// may persist the discovered `package.json` path so generated workflows can read it directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// `generic` only: the version field/key inside `manifest` (defaults to `version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_field: Option<String>,
    /// `generic` only: the shell command that publishes to the (unsupported) registry, e.g.
    /// `npx jsr publish`. Omitted ⇒ the package is build-only (artifacts -> GitHub Release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<String>,

    // --- build-only release packaging (used by `github-release`) ---
    /// Package each staged binary into an archive before attaching it: `tar.gz`, `zip`, or `auto`.
    /// Defaults to `auto` for build-only packages — see [`PackageEntry::archive_format`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveFormat>,
    /// Generate signed build provenance for every release asset, via GitHub's
    /// `actions/attest-build-provenance`. Consumers verify with `gh attestation verify <file>
    /// --repo <owner/repo>`.
    ///
    /// This is the only one of the three that establishes **authenticity**. `checksums` proves an
    /// asset arrived intact; provenance proves it was built by this repo's workflow from this
    /// commit — an attacker who replaces an asset can replace the checksum beside it, but cannot
    /// forge the signature. Off by default so `upgrade` never silently changes a workflow's
    /// permissions; `init` proposes enabling it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub attest: bool,
    /// Publish this npm package with `--provenance`: a signed statement, from the workflow's OIDC
    /// identity, of which repo and commit produced the tarball. npm shows it on the package page
    /// and `npm audit signatures` verifies it.
    ///
    /// The npm twin of [`attest`](Self::attest), which covers assets attached to a GitHub Release.
    /// Off by default for the same reason: turning it on changes the workflow's permissions, so it
    /// must be a decision rather than something `upgrade` does silently.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provenance: bool,
    /// Also attach a combined `checksums.txt` (SHA-256 of every asset) to the GitHub Release.
    #[serde(default, skip_serializing_if = "is_false")]
    pub checksums: bool,
    /// Extra files to bundle **inside each archive** alongside the binary — repo-relative paths or
    /// globs, e.g. `["README.md", "LICENSE", "types/*.d.ts"]`. Ignored when `archive` is unset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Whether the staged artifact is stored executable (mode `755`) inside each archive.
    ///
    /// Unset infers it — see [`PackageEntry::marks_executable`] — which is right for a CLI binary
    /// and right for a brotli-compressed blob. Set it explicitly for the cases inference cannot
    /// know about: `false` for a build-only package shipping data (a `.wasm`, a `.jar`, a model
    /// file), `true` for a program the inference would otherwise skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<bool>,

    // --- release identity: what this package overrides from the repo-wide settings ---
    /// Tag format for this package alone, replacing the repo's `tag_format`. Same placeholders, and
    /// it likewise governs both the tag written and the tags read back as this package's history.
    ///
    /// A monorepo is usually one release line, but a polyglot repo often is not: a Rust workspace
    /// shipping a CLI under `v{version}` — the tag its installer and self-updater read — alongside
    /// independently versioned npm packages needs the two kept apart. Two packages formatting to
    /// the same tag is not a warning, it is a silently skipped release: `github-release` treats an
    /// existing release as already shipped, so the second package attaches nothing at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_format: Option<String>,
    /// Older formats to read as *this package's* release history.
    ///
    /// Declared here rather than repo-wide when the old format carries no `{name}`: `v{version}`
    /// matches `v0.23.0` for every package that asks, so a repo-wide entry hands one package's
    /// history to all of them — including packages that have never been released, which then stop
    /// looking like first releases. Naming it here scopes it to the package that actually owned
    /// those tags. When present, this replaces `legacy_tag_formats` for this package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_tag_formats: Vec<String>,
    /// Changelog file for this package alone, relative to the repo root, replacing whatever
    /// `changelog_scope` and the adapter would have chosen. The escape for a package whose notes do
    /// not belong where its versioning would put them — a second binary that inherits a lockstep
    /// workspace version but keeps release notes of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

impl PackageEntry {
    /// Whether this package ships its artifacts via a GitHub Release instead of a registry.
    ///
    /// `build-only` means "standalone binaries attached to a GitHub Release" — correct for a cargo
    /// or generic CLI. It is **meaningless for an npm matrix package**, whose per-platform binaries
    /// ship *inside the npm tarball* under `bin/<stage_as>/`, not as Release assets. So an
    /// npm + matrix package is always treated as `publish` regardless of its stored mode, which is
    /// what keeps its binaries flowing to `npm publish` instead of a cosmetic GitHub Release.
    pub fn is_build_only(&self) -> bool {
        self.mode == Mode::BuildOnly && !(self.adapter == Ecosystem::Npm && self.matrix)
    }

    /// How `github-release` should package this package's binaries.
    ///
    /// Build-only packages default to [`ArchiveFormat::Auto`] when `archive` is unset: an archive
    /// is the convention every consumer already expects (`.zip` on Windows, `.tar.gz` elsewhere),
    /// it preserves the executable bit a bare binary download loses, and it is the only shape that
    /// can carry `include` files. A raw binary is not currently reachable — that opt-out is a
    /// deliberate future addition, not an oversight.
    pub fn archive_format(&self) -> Option<ArchiveFormat> {
        self.archive
            .or_else(|| self.is_build_only().then_some(ArchiveFormat::Auto))
    }

    /// Whether the staged artifact is stored executable inside its release archive.
    ///
    /// Defaults to "yes, unless the artifact is compressed": a `compress = "brotli"` package stages
    /// a `.br` blob the install step decompresses, which is data rather than a program. An explicit
    /// `executable` in `release.toml` overrides that inference in either direction.
    ///
    /// Defaulting to *not* executable would be the wrong safe-looking choice — it would silently
    /// ship archives whose binary needs a `chmod +x`, which is the bug this behavior exists to fix.
    pub fn marks_executable(&self) -> bool {
        self.executable.unwrap_or(self.compress.is_none())
    }

    /// Reject a `tag_format` that cannot produce a distinct tag, or a `changelog` path that would
    /// be written outside the repo. Both are joined onto the repo root and acted on at release
    /// time, so a bad value must fail while `release.toml` is being read.
    pub fn validate_release_identity(&self) -> Result<()> {
        if let Some(format) = &self.tag_format {
            format_tag(format, &self.name, "1.2.3")
                .with_context(|| format!("package `{}`: tag_format", self.name))?;
        }
        for format in &self.legacy_tag_formats {
            format_tag(format, &self.name, "1.2.3")
                .with_context(|| format!("package `{}`: legacy_tag_formats", self.name))?;
        }
        if let Some(changelog) = &self.changelog {
            let path = Path::new(changelog);
            if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
                bail!(
                    "package `{}`: changelog must be a path inside the repo, relative to its root \
                     (got `{changelog}`)",
                    self.name
                );
            }
        }
        Ok(())
    }

    /// The inverse of [`is_build_only`]: the package is published to its registry.
    pub fn is_publish(&self) -> bool {
        !self.is_build_only()
    }

    /// An npm publish package whose build runs **inline** in its own publish job (`npm run build`
    /// on the same runner, right before `npm publish`), instead of a separate build job that
    /// uploads artifacts for a later download. This is the tool-owned-build convention for plain
    /// npm libraries: no cross-job staging is needed because npm packs the freshly built output in
    /// place. Matrix npm packages ship per-platform binaries and still stage across jobs; cargo and
    /// generic packages build through their own publish path, so neither builds inline.
    pub fn builds_inline(&self) -> bool {
        (self.adapter == Ecosystem::Npm || self.adapter == Ecosystem::Jsr)
            && !self.matrix
            && !self.command.trim().is_empty()
    }
}

/// Global lifecycle hook scripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    /// Commands to run before computing the release (e.g. `npm run lint`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_version: Vec<String>,
    /// Commands to run after versions/manifests are updated but before committing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_version: Vec<String>,
    /// Commands to run before publishing starts in CI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_publish: Vec<String>,
    /// Commands to run after a successful publish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_publish: Vec<String>,
}

/// A step emitted into every job the workflow runs, before the work that job does.
///
/// The escape hatch for a pipeline that needs a tool the runner does not ship and no adapter knows
/// about — a task runner, a bundler installed by its own `install.sh`, a private toolchain. Such a
/// tool cannot be installed by a [hook](Hooks): hooks are executed by `otf-release publish` at
/// runtime, which is *after* the build step in the same job, so nothing a hook does can provision
/// what the build already needed. It is also the wrong way round — the hooks are themselves
/// written in the tool a setup step installs.
///
/// Folding the install into [`command`](PackageEntry::command) works — the string is emitted
/// verbatim into `run:` — but it is the wrong home for it twice over. `command` is also what
/// `otf-release build` runs on a contributor's machine, so an installer buried there executes
/// outside CI; and everything crammed into one `run:` block cannot use `$GITHUB_PATH`, whose writes
/// only reach *later* steps. A setup step is a step of its own that precedes the build, so
/// `echo … >> "$GITHUB_PATH"` behaves normally and a composite action's PATH exports reach the
/// build — the same mechanism that already works in a hand-written workflow.
///
/// ```toml
/// [setup]                                  # every job in the repo
/// uses = "./.github/actions/setup-tsr"
/// with = { esdev = "true" }
/// ```
///
/// `uses` and `run` are independent: either alone, or both (the action first, then the script).
///
/// Repo-wide, with no per-package override: the tool a repo builds and publishes through is a
/// property of the repo, and every job needs it on the same terms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// An action to run, exactly as a workflow would reference it: a local composite action
    /// (`./.github/actions/setup-tsr`) or a published one (`owner/repo@v1`).
    ///
    /// A local path is the common case and the reason this field exists rather than a list of
    /// shell lines: the repo usually already has the action, driving its other workflows, and
    /// re-spelling its installer here would fork the very definition it exists to keep single.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    /// Inputs for `uses`, emitted as the step's `with:` block. Ordered, so regenerating an
    /// unchanged config cannot reorder the workflow.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, String>,
    /// Shell lines run as a step, for a repo with no composite action to point at. Emitted as one
    /// multi-line `run:` block, so `$GITHUB_PATH` writes here reach the build step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
}

impl Setup {
    /// Whether this setup emits nothing. `with` alone does not count — it configures `uses`.
    pub fn is_empty(&self) -> bool {
        self.uses.is_none() && self.run.is_empty()
    }

    /// Parse `with` inputs written as `key=value` pairs on one line, the shape both `init` and the
    /// config editor collect them in. Blank entries are skipped so a trailing comma is harmless.
    pub fn parse_with(input: &str) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for pair in input.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .with_context(|| format!("`{pair}` is not a key=value pair"))?;
            let key = key.trim();
            if key.is_empty() {
                bail!("`{pair}` has no input name before the `=`");
            }
            out.insert(key.to_string(), value.trim().to_string());
        }
        Ok(out)
    }

    /// `with` rendered back into the one-line `key=value` form [`parse_with`](Self::parse_with)
    /// reads, so the editor shows what it will accept.
    pub fn format_with(&self) -> String {
        self.with
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Reject a shape that would generate invalid or misleading YAML.
    pub fn validate(&self, context: &str) -> Result<()> {
        if !self.with.is_empty() && self.uses.is_none() {
            bail!("{context}: `with` configures `uses`, but no `uses` is set");
        }
        if let Some(uses) = &self.uses {
            if uses.trim().is_empty() {
                bail!("{context}: `uses` cannot be blank");
            }
        }
        if self.run.iter().any(|line| line.trim().is_empty()) {
            bail!("{context}: `run` cannot contain a blank line");
        }
        Ok(())
    }
}

/// Publish policy knobs that affect release gating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishConfig {
    /// Per-package path globs that publish flow checks should ignore when deciding whether path-scoped
    /// commits without changelog notes deserve only a warning.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ignore_paths: HashMap<String, Vec<String>>,
}

/// The path globs a new package starts with in `publish.ignore_paths`.
///
/// Preflight refuses to release a package that has commits since its last tag but no
/// `[Unreleased]` notes. That rule is right for code and wrong for a README fix or a test-only
/// change, which is the whole reason `ignore_paths` exists — and seeding it empty (what `init` used
/// to do) meant every repo hit the false alarm before discovering the setting.
///
/// Documentation is common to every ecosystem; the test layout is not, so it comes from the
/// adapter that owns the package. These are a starting point, not a policy: they are written into
/// `release.toml` as plain globs precisely so a repo can edit them.
pub fn default_ignore_paths(ecosystem: Ecosystem) -> Vec<String> {
    let globs: &[&str] = match ecosystem {
        // `__tests__` (jest), `*.test.*`/`*.spec.*` (jest/vitest), `test/`+`tests/` (node:test, ava).
        Ecosystem::Npm => &[
            "**/*.md",
            "**/__tests__/**",
            "**/*.test.*",
            "**/*.spec.*",
            "**/test/**",
            "**/tests/**",
        ],
        // Cargo's own layout: integration tests and benches live outside `src/`. Unit tests sit
        // *inside* `src/`, so they are deliberately not covered — a `#[cfg(test)]` change usually
        // rides along with the code it tests.
        Ecosystem::Cargo => &["**/*.md", "**/tests/**", "**/benches/**"],
        // Deno's convention is `_test.ts`; `.test.ts` is accepted too.
        Ecosystem::Jsr => &["**/*.md", "**/*_test.ts", "**/*.test.ts", "**/tests/**"],
        // Nothing can be assumed about the layout, so only documentation.
        Ecosystem::Generic => &["**/*.md"],
    };
    globs.iter().map(|glob| (*glob).to_string()).collect()
}

/// Repository-secret names the generated workflow reads for registry auth.
///
/// Hardcoded names meant an org with a naming convention, or one publishing to a registry with
/// different credentials, had to hand-edit generated YAML — and then remember never to regenerate
/// it. The defaults are unchanged, so an existing repo sees no difference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Secrets {
    /// Repository secret holding the npm auth token, exposed as `NODE_AUTH_TOKEN`.
    #[serde(default = "default_npm_secret")]
    pub npm: String,
    /// Repository secret holding the crates.io token, exposed as `CARGO_REGISTRY_TOKEN`.
    #[serde(default = "default_cargo_secret")]
    pub cargo: String,
}

fn default_npm_secret() -> String {
    "NPM_TOKEN".to_string()
}

fn default_cargo_secret() -> String {
    "CARGO_REGISTRY_TOKEN".to_string()
}

impl Default for Secrets {
    fn default() -> Self {
        Self {
            npm: default_npm_secret(),
            cargo: default_cargo_secret(),
        }
    }
}

impl Secrets {
    /// Whether this is the default naming, so it can be omitted when serialising.
    fn is_default(&self) -> bool {
        *self == Secrets::default()
    }
}

/// Where an ecosystem's packages live, when the repo does not declare that natively.
///
/// npm discovery normally reads the root `package.json`'s `workspaces` globs. A polyglot repo
/// often has no root `package.json` at all — the root belongs to another ecosystem (a Cargo
/// workspace, say) and the JS packages are independent projects with their own lockfiles. Adding
/// a root `workspaces` declaration purely to satisfy this tool is not a neutral edit: npm, pnpm,
/// and bun all *act* on it, hoisting every member into one root `node_modules` behind a single
/// lockfile. So the declaration lives here instead — same determinism, no effect on how the repo
/// installs.
///
/// Entries are globs relative to the repo root naming package *directories* (`packages/*`,
/// `types`), not manifest files. `init` and `config` write them from what the repo scan found and
/// you confirmed; discovery never guesses at release time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Discovery {
    /// Explicit npm package directories. Non-empty ⇒ these *are* the members, and the root
    /// `package.json` is not consulted for `workspaces`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub npm: Vec<String>,
}

impl Discovery {
    /// Nothing declared for any ecosystem — the table is omitted from `release.toml` entirely.
    pub fn is_empty(&self) -> bool {
        self.npm.is_empty()
    }
}

/// The whole `release.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseConfig {
    /// Ecosystems enabled for this repo.
    pub adapters: Vec<Ecosystem>,
    /// Which `otf-release` release the generated workflow installs, as a git tag (e.g. `v0.25.0`).
    ///
    /// Defaults to the version of the binary that generated the workflow, which for a normal repo
    /// is exactly right — you installed a released build, so it exists. Set it explicitly when that
    /// assumption breaks: most notably this repo, which generates its own workflow from an
    /// unreleased working tree, so the default would pin to a tag that does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otf_release_version: Option<String>,
    /// Publishable packages that this tool must not version or publish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_publish: Vec<String>,
    /// Global lifecycle hooks.
    #[serde(default)]
    pub hooks: Hooks,
    /// A setup step run before **every** package's build, for tooling the whole repo builds
    /// through. A package can replace it with [`PackageEntry::setup`].
    #[serde(default, skip_serializing_if = "Setup::is_empty")]
    pub setup: Setup,
    /// Publish path-ignore policy keyed by package name.
    #[serde(default)]
    pub publish: PublishConfig,
    /// Names of the repository secrets the generated workflow reads for registry auth.
    #[serde(default, skip_serializing_if = "Secrets::is_default")]
    pub secrets: Secrets,
    /// Explicit package locations for ecosystems whose members this repo does not declare
    /// natively. Empty for a repo whose root manifest already declares them.
    #[serde(default, skip_serializing_if = "Discovery::is_empty")]
    pub discovery: Discovery,
    /// Packages with an explicit build step. Packages absent here are published as-is by their
    /// adapter (no build), in `publish` mode.
    #[serde(default, rename = "package")]
    pub packages: Vec<PackageEntry>,
    /// Tag used for automated snapshot releases (e.g. "snapshot", "canary").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_tag: Option<String>,
    /// Git tag format for releases. Supports `{version}` and optional `{name}` placeholders.
    #[serde(default = "default_tag_format")]
    pub tag_format: String,
    /// Older tag formats to read as release history while writing new tags with `tag_format`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_tag_formats: Vec<String>,
    /// Git hosting provider (e.g. "github", "gitlab").
    #[serde(default = "default_provider")]
    pub provider: String,
    /// The branch a release is started from and returned to (e.g. `main`, `master`, `trunk`).
    #[serde(default = "default_branch")]
    pub default_branch: String,
    /// How the changelog is managed.
    #[serde(default)]
    pub changelog_strategy: ChangelogStrategy,
    /// Where curated changelog notes are maintained.
    #[serde(default)]
    pub changelog_scope: ChangelogScope,
    /// How GitHub Release bodies are generated in CI.
    #[serde(default)]
    pub github_release_notes: GithubReleaseNotes,
}

/// The strategy for managing changelogs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangelogStrategy {
    /// Read [Unreleased] sections from hand-written CHANGELOG.md files.
    #[default]
    Curated,
    /// Automatically generate from Git commits since the last tag.
    Generated,
}

/// Where release notes live in a repository.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangelogScope {
    /// A single root CHANGELOG.md is shared by every package.
    Root,
    /// Each package uses the changelog path discovered by its adapter.
    #[default]
    Package,
}

fn default_provider() -> String {
    "github".to_string()
}

fn default_branch() -> String {
    DEFAULT_BRANCH.to_string()
}

fn default_tag_format() -> String {
    DEFAULT_TAG_FORMAT.to_string()
}

impl Default for ReleaseConfig {
    fn default() -> Self {
        Self {
            otf_release_version: None,
            adapters: Vec::new(),
            skip_publish: Vec::new(),
            hooks: Hooks::default(),
            setup: Setup::default(),
            publish: PublishConfig::default(),
            secrets: Secrets::default(),
            discovery: Discovery::default(),
            packages: Vec::new(),
            snapshot_tag: None,
            tag_format: default_tag_format(),
            legacy_tag_formats: Vec::new(),
            provider: default_provider(),
            default_branch: default_branch(),
            changelog_strategy: ChangelogStrategy::default(),
            changelog_scope: ChangelogScope::default(),
            github_release_notes: GithubReleaseNotes::default(),
        }
    }
}

/// Every package's tag format, resolved once and carried to the commands that write or read tags.
///
/// Commands take this rather than a bare format string so a per-package override can never be
/// applied in one place and forgotten in another — `check`, `publish`, `github-release`, and the
/// history lookups behind `version`/preflight all resolve through the same value.
#[derive(Debug, Clone, PartialEq)]
pub struct TagFormats {
    global: String,
    legacy: Vec<String>,
    per_package: HashMap<String, String>,
    per_package_legacy: HashMap<String, Vec<String>>,
}

impl TagFormats {
    /// One format for every package — the shape of a repo with no overrides, and what tests and
    /// the snapshot flow build directly.
    pub fn global(format: &str) -> Self {
        Self {
            global: format.to_string(),
            legacy: Vec::new(),
            per_package: HashMap::new(),
            per_package_legacy: HashMap::new(),
        }
    }

    /// Add the formats to read as history alongside whichever format applies (`legacy_tag_formats`).
    pub fn with_legacy(mut self, legacy: Vec<String>) -> Self {
        self.legacy = legacy;
        self
    }

    /// The format used to *write* this package's tag.
    pub fn for_package(&self, pkg_name: &str) -> &str {
        self.per_package
            .get(pkg_name)
            .map(String::as_str)
            .unwrap_or(&self.global)
    }

    /// The formats used to *read* this package's release history: its own format first, then the
    /// repo's legacy formats. A package with an override deliberately does not fall back to the
    /// global format — that is the tag line it was moved out of, and matching it again would hand
    /// this package another package's tags as its own history.
    /// A package that names its own legacy formats uses those *instead of* the repo-wide list.
    /// That is the escape hatch for a nameless old format: put `v{version}` on the one package
    /// whose tags it wrote, and every other package correctly reads as having no history under it.
    pub fn history_for(&self, pkg_name: &str) -> Vec<String> {
        let legacy = self
            .per_package_legacy
            .get(pkg_name)
            .unwrap_or(&self.legacy);
        std::iter::once(self.for_package(pkg_name).to_string())
            .chain(legacy.iter().cloned())
            .collect()
    }

    /// Format this package's tag at `version`.
    pub fn tag_for(&self, pkg_name: &str, version: &str) -> Result<String> {
        format_tag(self.for_package(pkg_name), pkg_name, version)
    }
}

/// Where each package's curated notes live, resolved once from `changelog_scope` plus any
/// per-package `changelog` override.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangelogLayout {
    scope: ChangelogScope,
    per_package: HashMap<String, String>,
}

impl ChangelogLayout {
    /// One scope for every package, with no overrides.
    pub fn scoped(scope: ChangelogScope) -> Self {
        Self {
            scope,
            per_package: HashMap::new(),
        }
    }

    /// The changelog path for a package, or `None` to keep the one its adapter discovered.
    ///
    /// Precedence: an explicit override wins; otherwise root scope forces the root file and package
    /// scope defers to the adapter (which is what already gives a lockstep Cargo workspace's
    /// inheriting crates the root `CHANGELOG.md` and everything else its own).
    pub fn path_for(&self, root: &Path, pkg_name: &str) -> Option<PathBuf> {
        if let Some(path) = self.per_package.get(pkg_name) {
            return Some(root.join(path));
        }
        match self.scope {
            ChangelogScope::Root => Some(root.join("CHANGELOG.md")),
            ChangelogScope::Package => None,
        }
    }
}

pub fn format_tag(format: &str, name: &str, version: &str) -> Result<String> {
    if !format.contains("{version}") {
        bail!("tag_format must contain `{{version}}`");
    }
    Ok(format.replace("{name}", name).replace("{version}", version))
}

impl ReleaseConfig {
    /// Tag formats used to find prior releases. New tags are still written only with `tag_format`.
    pub fn history_tag_formats(&self) -> Vec<String> {
        std::iter::once(self.tag_format.clone())
            .chain(self.legacy_tag_formats.iter().cloned())
            .collect()
    }

    /// The repo's tag formats: the global one plus whatever individual `[[package]]` blocks set.
    pub fn tag_formats(&self) -> TagFormats {
        TagFormats {
            global: self.tag_format.clone(),
            legacy: self.legacy_tag_formats.clone(),
            per_package: self
                .packages
                .iter()
                .filter_map(|pkg| Some((pkg.name.clone(), pkg.tag_format.clone()?)))
                .collect(),
            per_package_legacy: self
                .packages
                .iter()
                .filter(|pkg| !pkg.legacy_tag_formats.is_empty())
                .map(|pkg| (pkg.name.clone(), pkg.legacy_tag_formats.clone()))
                .collect(),
        }
    }

    /// The repo's changelog placement: the global scope plus whatever individual `[[package]]`
    /// blocks name explicitly.
    pub fn changelog_layout(&self) -> ChangelogLayout {
        ChangelogLayout {
            scope: self.changelog_scope.clone(),
            per_package: self
                .packages
                .iter()
                .filter_map(|pkg| Some((pkg.name.clone(), pkg.changelog.clone()?)))
                .collect(),
        }
    }

    /// The setup step every job runs, or `None` when none is configured.
    pub fn setup_step(&self) -> Option<&Setup> {
        (!self.setup.is_empty()).then_some(&self.setup)
    }

    /// The `[[package]]` block for a name, if the repo declares one.
    pub fn package(&self, name: &str) -> Option<&PackageEntry> {
        self.packages.iter().find(|pkg| pkg.name == name)
    }

    /// Reject a package block whose release identity would produce an unusable tag or escape the
    /// repo. Called on load, so a hand-edited `release.toml` fails at parse time rather than
    /// mid-release.
    fn validate_packages(&self) -> Result<()> {
        self.setup.validate("[setup]")?;
        for pkg in &self.packages {
            pkg.validate_release_identity()?;
        }
        Ok(())
    }

    /// The configured publish ignore globs for this package name.
    pub fn publish_ignore_paths_for(&self, pkg_name: &str) -> &[String] {
        self.publish
            .ignore_paths
            .get(pkg_name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Mark configured packages as non-publishable so version/preflight/publish treat them like
    /// private apps without requiring package manifests to set `private: true`.
    pub fn apply_publish_skips(&self, packages: &mut [crate::adapter::Pkg]) {
        for pkg in packages {
            if self.skip_publish.iter().any(|name| name == &pkg.name) {
                pkg.publishable = false;
            }
        }
    }

    /// The path to `release.toml` under `root`.
    pub fn path(root: &Path) -> PathBuf {
        root.join(CONFIG_FILE)
    }

    /// Whether a `release.toml` exists under `root`.
    pub fn exists(root: &Path) -> bool {
        Self::path(root).exists()
    }

    /// Load and parse `release.toml`. The error names the file when it is missing.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {} — run `otf-release init` to create it",
                path.display()
            )
        })?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config
            .validate_packages()
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
    }

    /// Serialize to `release.toml` under `root`.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        let text = toml::to_string_pretty(self)
            .with_context(|| format!("serializing {}", path.display()))?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Names of all `build-only` packages — the set `publish` must skip (they ship via the
    /// GitHub Release the workflow creates, not through a registry).
    pub fn build_only_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .packages
            .iter()
            .filter(|p| p.is_build_only())
            .map(|p| p.name.clone())
            .collect();
        names.extend(self.skip_publish.iter().cloned());
        names
    }

    /// Names of `matrix` publish-mode packages — those that must have their per-platform binaries
    /// staged before `publish` is allowed to push them (see `PublishOptions::require_staged`).
    pub fn matrix_publish_names(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.matrix && p.is_publish())
            .map(|p| p.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musl_targets_resolve_distinctly_from_glibc() {
        // musl is keyed under its own OS name so it never collides with the glibc linux rows.
        let musl = Target::resolved("linux-musl", "x86_64");
        assert_eq!(musl.triple(), "x86_64-unknown-linux-musl");
        assert_eq!(musl.runner(), "ubuntu-latest");
        assert_eq!(musl.stage_as(), "linux-musl-x64");
        assert!(!musl.is_cross());

        let musl_arm = Target::resolved("linux-musl", "aarch64");
        assert_eq!(musl_arm.triple(), "aarch64-unknown-linux-musl");
        assert_eq!(musl_arm.stage_as(), "linux-musl-arm64");
        assert!(musl_arm.is_cross()); // cross-linked on the x64 runner

        // The glibc row for the same (os-ish, arch) is untouched.
        assert_eq!(
            Target::resolved("linux", "x86_64").triple(),
            "x86_64-unknown-linux-gnu"
        );

        // Neither musl target is selected by `init` unless explicitly opted in.
        for info in TARGET_REGISTRY.iter().filter(|t| t.name == "linux-musl") {
            assert!(!info.default_on, "{} should be opt-in", info.label);
        }
    }

    #[test]
    fn freebsd_targets_build_in_a_vm_not_by_cross_compiling() {
        let bsd = Target::resolved("freebsd", "x86_64");
        assert_eq!(bsd.triple(), "x86_64-unknown-freebsd");
        assert_eq!(bsd.runner(), "ubuntu-latest"); // the *host*; the build happens in the guest
        assert_eq!(bsd.stage_as(), "freebsd-x64"); // valid Node process.platform-arch

        // `vm` and `cross` are mutually exclusive here: cross-compiling FreeBSD from Linux needs
        // base-system libs Rust does not ship, and the GNU/Linux cross prep is the wrong toolchain.
        assert!(bsd.is_vm());
        assert!(!bsd.is_cross());

        // aarch64 is tier 3 with no prebuilt std, so it *only* works as the guest's host target.
        let arm = Target::resolved("freebsd", "aarch64");
        assert_eq!(arm.triple(), "aarch64-unknown-freebsd");
        assert!(arm.is_vm());
        assert!(!arm.is_cross());

        for info in TARGET_REGISTRY.iter().filter(|t| t.name == "freebsd") {
            assert!(!info.default_on, "{} should be opt-in", info.label);
        }
        // Every natively-hosted target stays a host build — VM prep is FreeBSD-only for now.
        for info in TARGET_REGISTRY.iter().filter(|t| t.name != "freebsd") {
            assert!(!info.vm, "{} should build on the host", info.label);
        }
    }

    /// The default must be "executable" for an ordinary binary: getting this backwards ships
    /// archives that need a `chmod +x`, which is the bug the mode override exists to prevent.
    #[test]
    fn executable_defaults_to_inference_and_is_overridable_both_ways() {
        let mut entry = PackageEntry {
            name: "esrun".into(),
            adapter: Ecosystem::Cargo,
            mode: Mode::BuildOnly,
            matrix: true,
            targets: Vec::new(),
            command: String::new(),
            artifacts: String::new(),
            bin_name: Some("esrun".into()),
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            provenance: false,
            include: Vec::new(),
            tag_format: None,
            legacy_tag_formats: Vec::new(),
            changelog: None,
            executable: None,
        };

        // Unset + raw binary ⇒ executable.
        assert!(entry.marks_executable());

        // Unset + brotli ⇒ the staged `.br` is data, not a program.
        entry.compress = Some("brotli".into());
        assert!(!entry.marks_executable());

        // An explicit value wins over the inference in both directions.
        entry.executable = Some(true);
        assert!(entry.marks_executable());
        entry.compress = None;
        entry.executable = Some(false);
        assert!(!entry.marks_executable(), "for a .wasm/.jar-style payload");
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = ReleaseConfig {
            discovery: Default::default(),
            otf_release_version: None,
            snapshot_tag: None,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            legacy_tag_formats: Vec::new(),
            skip_publish: vec!["private-tool".to_string()],
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            hooks: Hooks::default(),
            setup: Default::default(),
            publish: PublishConfig {
                ignore_paths: HashMap::from([(
                    "docs-site".into(),
                    vec!["docs/**".into(), "**/*.test.ts".into()],
                )]),
            },
            secrets: Default::default(),
            packages: vec![
                PackageEntry {
                    name: "web-compiler".into(),
                    adapter: Ecosystem::Cargo,
                    mode: Mode::BuildOnly,
                    matrix: true,
                    targets: vec![Target::resolved("linux", "x86_64")],
                    command: "cargo build --release -p otfw_cli".into(),
                    artifacts: "target/*/release/otfwc*".into(),
                    bin_name: Some("otfwc".into()),
                    compress: None,
                    manifest: None,
                    version_field: None,
                    publish: None,
                    archive: Some(ArchiveFormat::Auto),
                    checksums: true,
                    attest: false,
                    provenance: false,
                    executable: None,
                    include: vec!["README.md".into(), "LICENSE".into()],
                    tag_format: None,
                    legacy_tag_formats: Vec::new(),
                    changelog: None,
                },
                PackageEntry {
                    name: "docs-site".into(),
                    adapter: Ecosystem::Npm,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: vec![],
                    command: "npm run build".into(),
                    artifacts: "dist/**".into(),
                    bin_name: None,
                    compress: None,
                    manifest: None,
                    version_field: None,
                    publish: None,
                    archive: None,
                    checksums: false,
                    attest: false,
                    provenance: false,
                    executable: None,
                    include: Vec::new(),
                    tag_format: None,
                    legacy_tag_formats: Vec::new(),
                    changelog: None,
                },
            ],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        // Registry names, not Rust identifiers.
        assert!(text.contains("\"npm\""));
        assert!(text.contains("\"crates.io\""));
        assert!(text.contains("adapter = \"crates.io\""));
        assert!(text.contains("mode = \"build-only\""));
        assert!(text.contains("mode = \"publish\""));
        assert!(text.contains("github_release_notes = \"auto-generate\""));
        assert!(text.contains("skip_publish = [\"private-tool\"]"));
        assert!(text.contains("[publish.ignore_paths]"));
        // Build-only packaging fields serialize with their documented spellings.
        assert!(text.contains("archive = \"auto\""));
        assert!(text.contains("checksums = true"));
        assert!(text.contains("include = ["));
        assert!(text.contains("\"README.md\""));

        let back: ReleaseConfig = toml::from_str(&text).unwrap();
        let web = back
            .packages
            .iter()
            .find(|p| p.name == "web-compiler")
            .unwrap();
        assert_eq!(web.archive, Some(ArchiveFormat::Auto));
        assert!(web.checksums);
        assert_eq!(web.include, vec!["README.md", "LICENSE"]);
        // A package that sets none of them omits them entirely (defaults, not written).
        let docs = back
            .packages
            .iter()
            .find(|p| p.name == "docs-site")
            .unwrap();
        assert_eq!(docs.archive, None);
        assert!(!docs.checksums);
        assert!(docs.include.is_empty());
        assert_eq!(back.adapters, cfg.adapters);
        assert_eq!(back.github_release_notes, GithubReleaseNotes::AutoGenerate);
        assert_eq!(back.skip_publish, vec!["private-tool"]);
        assert_eq!(back.changelog_scope, ChangelogScope::Package);
        assert_eq!(
            back.publish_ignore_paths_for("docs-site"),
            ["docs/**", "**/*.test.ts"]
        );
        assert_eq!(back.packages.len(), 2);
        assert_eq!(
            back.build_only_names(),
            vec!["web-compiler".to_string(), "private-tool".to_string()]
        );
    }

    #[test]
    fn save_and_load_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ReleaseConfig {
            discovery: Default::default(),
            otf_release_version: None,
            snapshot_tag: None,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            legacy_tag_formats: Vec::new(),
            skip_publish: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Cargo],
            hooks: Hooks::default(),
            setup: Default::default(),
            publish: PublishConfig::default(),
            secrets: Default::default(),
            packages: vec![],
        };
        cfg.save(tmp.path()).unwrap();
        assert!(ReleaseConfig::exists(tmp.path()));
        let back = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(back.adapters, vec![Ecosystem::Cargo]);
    }

    /// A `[[package]]` block carrying nothing but identity — what `init` writes for a package its
    /// adapter publishes as-is.
    fn entry(name: &str) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Npm,
            mode: Mode::Publish,
            matrix: false,
            targets: Vec::new(),
            command: String::new(),
            artifacts: String::new(),
            bin_name: None,
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            provenance: false,
            include: Vec::new(),
            executable: None,
            tag_format: None,
            legacy_tag_formats: Vec::new(),
            changelog: None,
        }
    }

    fn config_with(packages: Vec<PackageEntry>) -> ReleaseConfig {
        ReleaseConfig {
            tag_format: "v{version}".to_string(),
            packages,
            ..ReleaseConfig::default()
        }
    }

    #[test]
    fn a_package_tag_format_moves_only_that_package_off_the_repo_tag_line() {
        // The ES-Runtime shape: a product tag line an installer reads, plus sidecar packages that
        // version independently and must not land in it.
        let cfg = config_with(vec![
            entry("es-runtime-cli"),
            PackageEntry {
                tag_format: Some("{name}@{version}".to_string()),
                legacy_tag_formats: Vec::new(),
                ..entry("@scope/driver")
            },
        ]);
        let tags = cfg.tag_formats();

        assert_eq!(tags.tag_for("es-runtime-cli", "0.24.0").unwrap(), "v0.24.0");
        assert_eq!(
            tags.tag_for("@scope/driver", "0.1.0").unwrap(),
            "@scope/driver@0.1.0"
        );
        // A package with no block at all still follows the repo.
        assert_eq!(tags.tag_for("undeclared", "1.0.0").unwrap(), "v1.0.0");
    }

    #[test]
    fn a_scoped_package_does_not_read_the_global_tag_line_as_its_own_history() {
        // The whole point of scoping the format: `v{version}` matches *every* v-tag in the repo, so
        // keeping it as a fallback would hand this package the CLI's releases as its history and
        // resurrect the collision the setting exists to prevent.
        let cfg = ReleaseConfig {
            legacy_tag_formats: vec!["release-{version}".to_string()],
            ..config_with(vec![
                entry("es-runtime-cli"),
                PackageEntry {
                    tag_format: Some("{name}@{version}".to_string()),
                    legacy_tag_formats: Vec::new(),
                    ..entry("@scope/driver")
                },
            ])
        };
        let tags = cfg.tag_formats();

        assert_eq!(
            tags.history_for("@scope/driver"),
            vec![
                "{name}@{version}".to_string(),
                "release-{version}".to_string()
            ],
        );
        // A package that does not scope its format still reads the repo's line, legacy included.
        assert_eq!(
            tags.history_for("es-runtime-cli"),
            vec!["v{version}".to_string(), "release-{version}".to_string()],
        );
    }

    #[test]
    fn a_package_changelog_wins_over_either_scope() {
        let root = Path::new("/repo");
        let scoped = PackageEntry {
            changelog: Some("crates/dev-cli/CHANGELOG.md".to_string()),
            ..entry("es-dev-cli")
        };

        // Root scope: every other package is pinned to the root file, the scoped one is not.
        let mut cfg = config_with(vec![entry("es-runtime-cli"), scoped]);
        cfg.changelog_scope = ChangelogScope::Root;
        let layout = cfg.changelog_layout();
        assert_eq!(
            layout.path_for(root, "es-dev-cli"),
            Some(root.join("crates/dev-cli/CHANGELOG.md"))
        );
        assert_eq!(
            layout.path_for(root, "es-runtime-cli"),
            Some(root.join("CHANGELOG.md"))
        );

        // Package scope: the scoped path still wins, and everything else keeps whatever its adapter
        // discovered — which is what leaves a lockstep workspace's crates on the root file.
        cfg.changelog_scope = ChangelogScope::Package;
        let layout = cfg.changelog_layout();
        assert_eq!(
            layout.path_for(root, "es-dev-cli"),
            Some(root.join("crates/dev-cli/CHANGELOG.md"))
        );
        assert_eq!(layout.path_for(root, "es-runtime-cli"), None);
    }

    #[test]
    fn package_release_identity_round_trips_and_is_validated_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(vec![PackageEntry {
            tag_format: Some("{name}@{version}".to_string()),
            legacy_tag_formats: Vec::new(),
            changelog: Some("packages/driver/CHANGELOG.md".to_string()),
            ..entry("@scope/driver")
        }]);
        cfg.save(tmp.path()).unwrap();

        let back = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(back.packages, cfg.packages);

        // A tag format with no {version} would format every release to the same tag.
        let bad = "adapters = []\n[[package]]\nname = \"@scope/driver\"\nadapter = \"npm\"\n\
                   mode = \"publish\"\ntag_format = \"latest\"\n";
        fs::write(ReleaseConfig::path(tmp.path()), bad).unwrap();
        let err = ReleaseConfig::load(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("tag_format"), "{err:#}");

        // A changelog path must stay inside the repo — it is joined onto the root and written to.
        let escaping = "adapters = []\n[[package]]\nname = \"@scope/driver\"\nadapter = \"npm\"\n\
                        mode = \"publish\"\nchangelog = \"../elsewhere/CHANGELOG.md\"\n";
        fs::write(ReleaseConfig::path(tmp.path()), escaping).unwrap();
        let err = ReleaseConfig::load(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("inside the repo"), "{err:#}");
    }

    #[test]
    fn skip_publish_marks_packages_non_publishable_and_publish_skips_them() {
        let cfg = ReleaseConfig {
            otf_release_version: None,
            skip_publish: vec!["@scope/manual".to_string()],
            ..ReleaseConfig::default()
        };
        let mut packages = vec![
            crate::adapter::Pkg {
                name: "@scope/manual".to_string(),
                version: "1.0.0".to_string(),
                manifest_path: "packages/manual/package.json".into(),
                changelog_path: "packages/manual/CHANGELOG.md".into(),
                publishable: true,
                internal_deps: vec![],
            },
            crate::adapter::Pkg {
                name: "@scope/managed".to_string(),
                version: "1.0.0".to_string(),
                manifest_path: "packages/managed/package.json".into(),
                changelog_path: "packages/managed/CHANGELOG.md".into(),
                publishable: true,
                internal_deps: vec![],
            },
        ];

        cfg.apply_publish_skips(&mut packages);

        assert!(!packages[0].publishable);
        assert!(packages[1].publishable);
        assert_eq!(cfg.build_only_names(), vec!["@scope/manual"]);
    }

    /// A legacy format with no `{name}` matches any package's tag, so a repo-wide entry hands one
    /// package's history to every package — including one that has never shipped, which then stops
    /// reading as a first release. Naming it on the package that owned those tags scopes it.
    #[test]
    fn a_packages_own_legacy_formats_replace_the_repo_wide_list() {
        let mut config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: vec!["v{version}".to_string()],
            ..ReleaseConfig::default()
        };
        config.packages = vec![
            PackageEntry {
                name: "es-runtime-cli".to_string(),
                tag_format: Some("esrun@{version}".to_string()),
                // The crate whose tags `v0.23.0` actually was.
                legacy_tag_formats: vec!["v{version}".to_string()],
                ..entry("es-runtime-cli")
            },
            PackageEntry {
                name: "es-runtime-dev-cli".to_string(),
                tag_format: Some("esdev@{version}".to_string()),
                ..entry("es-runtime-dev-cli")
            },
        ];

        let tags = config.tag_formats();
        assert_eq!(
            tags.history_for("es-runtime-cli"),
            vec!["esrun@{version}".to_string(), "v{version}".to_string()]
        );
        // The new crate falls back to the repo-wide list, which is where the ambiguity lives — so
        // the fix is to keep that list empty and scope the old format to the crate that owned it.
        assert_eq!(
            tags.history_for("es-runtime-dev-cli"),
            vec!["esdev@{version}".to_string(), "v{version}".to_string()]
        );

        config.legacy_tag_formats.clear();
        let tags = config.tag_formats();
        assert_eq!(
            tags.history_for("es-runtime-cli"),
            vec!["esrun@{version}".to_string(), "v{version}".to_string()],
            "the package that owns the old tags keeps reading them"
        );
        assert_eq!(
            tags.history_for("es-runtime-dev-cli"),
            vec!["esdev@{version}".to_string()],
            "a package that never shipped under the old format must not inherit its history"
        );
    }

    /// A seeded glob that matches nothing is worse than no glob at all: it looks configured, and
    /// the release it was meant to unblock still fails. Check them against the paths they exist for.
    #[test]
    fn seeded_ignore_paths_match_the_files_they_are_meant_to_cover() {
        let matches = |ecosystem: Ecosystem, path: &str| {
            default_ignore_paths(ecosystem)
                .iter()
                .any(|glob| glob::Pattern::new(glob).unwrap().matches(path))
        };

        // Documentation, at the repo root and inside a package, for every ecosystem.
        for ecosystem in Ecosystem::ALL {
            assert!(matches(ecosystem, "README.md"), "{ecosystem:?}");
            assert!(
                matches(ecosystem, "packages/redis/README.md"),
                "{ecosystem:?}"
            );
            assert!(
                !matches(ecosystem, "packages/redis/src/index.ts"),
                "{ecosystem:?}"
            );
        }

        assert!(matches(Ecosystem::Npm, "packages/redis/src/pool.test.ts"));
        assert!(matches(Ecosystem::Npm, "packages/redis/__tests__/pool.ts"));
        assert!(matches(Ecosystem::Npm, "packages/redis/test/pool.ts"));

        assert!(matches(
            Ecosystem::Cargo,
            "crates/core/tests/publish_flow.rs"
        ));
        assert!(matches(Ecosystem::Cargo, "crates/core/benches/parse.rs"));
        // Unit tests live inside `src/` and ride along with the code they cover, so a change there
        // still needs notes.
        assert!(!matches(Ecosystem::Cargo, "crates/core/src/publish.rs"));

        assert!(matches(Ecosystem::Jsr, "mod_test.ts"));
        assert!(matches(Ecosystem::Jsr, "src/mod.test.ts"));

        // Generic knows only about docs — anything else would be a guess about someone's layout.
        assert_eq!(default_ignore_paths(Ecosystem::Generic), vec!["**/*.md"]);
    }

    #[test]
    fn publish_ignore_paths_default_to_empty() {
        let cfg = ReleaseConfig::default();
        assert!(cfg.publish_ignore_paths_for("missing").is_empty());
    }

    #[test]
    fn default_branch_defaults_to_main_and_round_trips_a_custom_value() {
        // Absent from the file → defaults to main.
        let cfg: ReleaseConfig = toml::from_str("adapters = [\"npm\"]\n").unwrap();
        assert_eq!(cfg.default_branch, "main");

        // Explicit value survives a save/load round-trip.
        let custom: ReleaseConfig =
            toml::from_str("adapters = [\"npm\"]\ndefault_branch = \"trunk\"\n").unwrap();
        assert_eq!(custom.default_branch, "trunk");
        let text = toml::to_string_pretty(&custom).unwrap();
        assert!(text.contains("default_branch = \"trunk\""));
    }

    #[test]
    fn load_missing_is_a_helpful_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ReleaseConfig::load(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("otf-release init"));
    }
}
