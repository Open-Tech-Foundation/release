//! `otf-release` — the CLI entry point.
//!
//! Wires command-line arguments to the orchestration in `opentf-release-core`. There is **no
//! `--adapter` flag**: which ecosystems are active is read from `release.toml` (written by
//! `init`), the committed source of truth. `init` is interactive and creates that file.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use opentf_release_core::adapter::Adapter;
use opentf_release_core::config::{Ecosystem, ReleaseConfig};
use opentf_release_core::init::AdapterFactory;
use opentf_release_core::{init, publish, version};

/// Builds the concrete ecosystem adapters from `opentf-release-adapters`.
struct CliAdapterFactory {
    root: PathBuf,
}

impl AdapterFactory for CliAdapterFactory {
    fn make(&self, ecosystem: Ecosystem) -> Box<dyn Adapter> {
        match ecosystem {
            Ecosystem::Npm => Box::new(opentf_release_adapters::npm::NpmAdapter::new(
                self.root.clone(),
            )),
            Ecosystem::Cargo => Box::new(opentf_release_adapters::cargo::CargoAdapter::new(
                self.root.clone(),
            )),
        }
    }
}

/// Curated-changelog, manual-bump release CLI for polyglot monorepos.
#[derive(Debug, Parser)]
#[command(name = "otf-release", version, about)]
struct Cli {
    /// Workspace root (defaults to the current directory).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Interactive, local: pick bumps, cascade, and open a release PR. Never touches `main`.
    Version {
        /// Compute and print the plan, but write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Allow first-release of packages that have no prior tag.
        #[arg(long)]
        first_release: bool,
    },
    /// Non-interactive, CI: publish changed packages in dependency order. Idempotent.
    Publish {
        /// Directory of staged binary artifacts (`.artifacts/`).
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
        /// Resolve the plan and print it, but do not publish.
        #[arg(long)]
        dry_run: bool,
    },
    /// Interactive setup: write `release.toml` and generate `.github/workflows/release.yml`.
    Init {
        /// Overwrite existing files without prompting.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));
    let factory = CliAdapterFactory { root: root.clone() };

    match cli.command {
        // `init` is the one command that doesn't read the config — it writes it.
        Command::Init { force } => init::run(&factory, &root, &init::InitOptions { force }),

        // Every other command reads `release.toml` and acts on each enabled ecosystem.
        Command::Version {
            dry_run,
            first_release,
        } => {
            let config = ReleaseConfig::load(&root)?;
            let opts = version::VersionOptions {
                dry_run,
                first_release,
            };
            for eco in &config.adapters {
                let adapter = factory.make(*eco);
                version::run(adapter.as_ref(), &root, &opts)?;
            }
            Ok(())
        }

        Command::Publish {
            artifacts_dir,
            dry_run,
        } => {
            let config = ReleaseConfig::load(&root)?;
            // build-only packages ship via the GitHub Release the workflow creates, never a
            // registry — so `publish` skips them.
            let skip = config.build_only_names();
            for eco in &config.adapters {
                let adapter = factory.make(*eco);
                publish::run(
                    adapter.as_ref(),
                    &root,
                    &publish::PublishOptions {
                        artifacts_dir: artifacts_dir.clone(),
                        dry_run,
                        skip: skip.clone(),
                    },
                )?;
            }
            Ok(())
        }
    }
}
