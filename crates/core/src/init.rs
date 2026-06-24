//! The `init` command — interactive `release.yml` generator.
//!
//! Generates exactly one `.github/workflows/release.yml`. There is **no persisted config** —
//! the generated YAML is the single source of truth. See `docs/commands/init.md`.
//!
//! The YAML rendering ([`render_workflow`]) is a pure function with golden tests; the
//! interactive choices go through the [`InitPrompt`] trait so the flow is testable.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crate::adapter::{Adapter, Pkg};

/// A sensible default cross-compile target set (each emitted with an `# edit me` marker).
pub const DEFAULT_TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

/// Options for an `init` run.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Overwrite an existing `release.yml` without prompting.
    pub force: bool,
}

/// A publishable package selected as needing prebuilt binary artifacts, with its targets.
#[derive(Debug, Clone)]
pub struct AssetPackage {
    pub name: String,
    pub targets: Vec<String>,
}

/// The interactive choices `init` needs.
pub trait InitPrompt {
    /// Which publishable packages need binary artifacts built before publish?
    fn select_asset_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>>;
    /// The cross-compile target triples for an asset package.
    fn target_triples(&self, pkg_name: &str) -> Result<Vec<String>>;
    /// Confirm overwriting an existing workflow (only asked when not `--force`).
    fn confirm_overwrite(&self, path: &Path) -> Result<bool>;
}

/// Wire up the real prompt and run the generator.
pub fn run(adapter: &dyn Adapter, root: &Path, opts: &InitOptions) -> Result<()> {
    orchestrate(adapter, &StdinInitPrompt, root, opts)
}

/// The testable core of `init`.
pub fn orchestrate(
    adapter: &dyn Adapter,
    prompt: &dyn InitPrompt,
    root: &Path,
    opts: &InitOptions,
) -> Result<()> {
    let packages = adapter.discover_packages()?;
    let publishable: Vec<&Pkg> = packages.iter().filter(|p| p.publishable).collect();

    let asset_names = prompt.select_asset_packages(&publishable)?;
    let mut assets = Vec::new();
    for name in &asset_names {
        assets.push(AssetPackage {
            name: name.clone(),
            targets: prompt.target_triples(name)?,
        });
    }

    let yaml = render_workflow(&assets);
    let path = root.join(".github/workflows/release.yml");

    if path.exists() && !opts.force && !prompt.confirm_overwrite(&path)? {
        println!("Left existing {} unchanged.", path.display());
        return Ok(());
    }

    fs::create_dir_all(path.parent().unwrap())
        .with_context(|| format!("creating {}", path.parent().unwrap().display()))?;
    fs::write(&path, yaml).with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

/// Render the single `release.yml`. A `build-matrix` job is emitted only when there are asset
/// packages; the `publish` job then `needs` it and downloads artifacts to `.artifacts/`.
pub fn render_workflow(assets: &[AssetPackage]) -> String {
    let has_assets = !assets.is_empty();
    let mut s = String::from("name: Release\n\non:\n  push:\n    branches: [main]\n\njobs:\n");

    if has_assets {
        s.push_str("  build-matrix:\n");
        s.push_str("    runs-on: ubuntu-latest  # edit me: choose a runner per target\n");
        s.push_str("    strategy:\n      matrix:\n        include:\n");
        for asset in assets {
            for target in &asset.targets {
                s.push_str(&format!(
                    "          - {{ package: \"{}\", target: \"{}\" }}  # edit me\n",
                    asset.name, target
                ));
            }
        }
        s.push_str("    steps:\n");
        s.push_str("      - uses: actions/checkout@v4\n");
        s.push_str("      - name: Build ${{ matrix.package }} for ${{ matrix.target }}\n");
        s.push_str("        run: |\n");
        s.push_str("          # edit me: build the binary for ${{ matrix.target }}\n");
        s.push_str("          echo \"building ${{ matrix.package }} (${{ matrix.target }})\"\n");
        s.push_str("      - uses: actions/upload-artifact@v4\n");
        s.push_str("        with:\n");
        s.push_str("          name: ${{ matrix.package }}-${{ matrix.target }}\n");
        s.push_str("          path: . # edit me: path to the built binary/artifacts\n");
        s.push('\n');
    }

    s.push_str("  publish:\n");
    if has_assets {
        s.push_str("    needs: build-matrix\n");
    }
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    s.push_str("        with:\n");
    s.push_str("          fetch-depth: 0\n");
    s.push_str("      - uses: actions/setup-node@v4\n");
    s.push_str("        with:\n");
    s.push_str("          node-version: 20\n");
    s.push_str("          registry-url: https://registry.npmjs.org\n");
    if has_assets {
        s.push_str("      - uses: actions/download-artifact@v4\n");
        s.push_str("        with:\n");
        s.push_str("          path: .artifacts\n");
    }
    s.push_str("      - run: npm ci\n");
    s.push_str("      - name: Publish\n");
    if has_assets {
        s.push_str("        run: otf-release publish --artifacts-dir .artifacts\n");
    } else {
        s.push_str("        run: otf-release publish\n");
    }
    s.push_str("        env:\n");
    s.push_str("          NODE_AUTH_TOKEN: ${{ secrets.NODE_AUTH_TOKEN }}\n");
    s.push_str("          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n");
    s
}

/// The real terminal prompt for `init`.
pub struct StdinInitPrompt;

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

impl InitPrompt for StdinInitPrompt {
    fn select_asset_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>> {
        println!("Publishable packages:");
        for (i, p) in publishable.iter().enumerate() {
            println!("  {}) {}", i + 1, p.name);
        }
        let line = read_line("Which need binary artifacts built before publish? (e.g. 1,2 or 'none'): ")?;
        if line.is_empty() || line.eq_ignore_ascii_case("none") {
            return Ok(Vec::new());
        }
        let mut selected = Vec::new();
        for token in line.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
            if let Ok(idx) = token.parse::<usize>() {
                if let Some(p) = publishable.get(idx.wrapping_sub(1)) {
                    selected.push(p.name.clone());
                }
            }
        }
        Ok(selected)
    }

    fn target_triples(&self, pkg_name: &str) -> Result<Vec<String>> {
        let defaults = DEFAULT_TARGETS.join(", ");
        let line = read_line(&format!(
            "Target triples for {pkg_name} [{defaults}]: "
        ))?;
        if line.is_empty() {
            return Ok(DEFAULT_TARGETS.iter().map(|s| s.to_string()).collect());
        }
        Ok(line
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    fn confirm_overwrite(&self, path: &Path) -> Result<bool> {
        let line = read_line(&format!("{} exists. Overwrite? (y/N): ", path.display()))?;
        Ok(matches!(line.to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const LIBS_ONLY: &str = "name: Release\n\non:\n  push:\n    branches: [main]\n\njobs:\n  publish:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n      - uses: actions/setup-node@v4\n        with:\n          node-version: 20\n          registry-url: https://registry.npmjs.org\n      - run: npm ci\n      - name: Publish\n        run: otf-release publish\n        env:\n          NODE_AUTH_TOKEN: ${{ secrets.NODE_AUTH_TOKEN }}\n          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n";

    struct FakeAdapter {
        packages: Vec<Pkg>,
    }
    impl Adapter for FakeAdapter {
        fn discover_packages(&self) -> Result<Vec<Pkg>> {
            Ok(self.packages.clone())
        }
        fn write_version(&self, _: &Pkg, _: &str) -> Result<()> {
            unreachable!()
        }
        fn update_dep_range(&self, _: &Pkg, _: &str, _: &str) -> Result<()> {
            unreachable!()
        }
        fn format_range(&self, _: &str) -> String {
            unreachable!()
        }
        fn resolve_workspace_links(&self, _: &Pkg) -> Result<()> {
            unreachable!()
        }
        fn update_lockfile(&self, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn dependent_bump(&self, _: crate::adapter::Bump, _: &crate::adapter::DepKind) -> crate::adapter::Bump {
            unreachable!()
        }
        fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
    }

    struct FakePrompt {
        assets: Vec<String>,
        targets: Vec<String>,
        overwrite: bool,
    }
    impl InitPrompt for FakePrompt {
        fn select_asset_packages(&self, _: &[&Pkg]) -> Result<Vec<String>> {
            Ok(self.assets.clone())
        }
        fn target_triples(&self, _: &str) -> Result<Vec<String>> {
            Ok(self.targets.clone())
        }
        fn confirm_overwrite(&self, _: &Path) -> Result<bool> {
            Ok(self.overwrite)
        }
    }

    fn pkg(name: &str, publishable: bool) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: PathBuf::from(format!("{name}/package.json")),
            changelog_path: PathBuf::from(format!("{name}/CHANGELOG.md")),
            publishable,
            internal_deps: vec![],
        }
    }

    #[test]
    fn renders_libs_only_workflow() {
        assert_eq!(render_workflow(&[]), LIBS_ONLY);
    }

    #[test]
    fn renders_asset_workflow_with_matrix_and_artifacts() {
        let assets = vec![AssetPackage {
            name: "@x/cli".into(),
            targets: vec![
                "x86_64-unknown-linux-gnu".into(),
                "aarch64-apple-darwin".into(),
            ],
        }];
        let out = render_workflow(&assets);
        assert!(out.contains("  build-matrix:\n"));
        assert!(out.contains(
            "          - { package: \"@x/cli\", target: \"x86_64-unknown-linux-gnu\" }  # edit me\n"
        ));
        assert!(out.contains("    needs: build-matrix\n"));
        assert!(out.contains("          path: .artifacts\n"));
        assert!(out.contains("        run: otf-release publish --artifacts-dir .artifacts\n"));
        // No matrix means no asset wiring in the libs-only output.
        assert!(!LIBS_ONLY.contains("build-matrix"));
    }

    #[test]
    fn orchestrate_writes_libs_only_when_no_assets_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("@x/lib", true), pkg("@x/app", false)],
        };
        let prompt = FakePrompt {
            assets: vec![],
            targets: vec![],
            overwrite: true,
        };
        orchestrate(&adapter, &prompt, tmp.path(), &InitOptions::default()).unwrap();
        let written =
            fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert_eq!(written, LIBS_ONLY);
    }

    #[test]
    fn orchestrate_respects_overwrite_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".github/workflows/release.yml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "SENTINEL").unwrap();

        let adapter = FakeAdapter {
            packages: vec![pkg("@x/lib", true)],
        };

        // Not forced + declines overwrite => file untouched.
        let decline = FakePrompt {
            assets: vec![],
            targets: vec![],
            overwrite: false,
        };
        orchestrate(&adapter, &decline, tmp.path(), &InitOptions::default()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "SENTINEL");

        // Forced => overwritten without asking.
        orchestrate(&adapter, &decline, tmp.path(), &InitOptions { force: true }).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), LIBS_ONLY);
    }
}
