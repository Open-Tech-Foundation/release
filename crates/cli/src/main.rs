//! `otf-release` — the CLI entry point.
//!
//! Wires command-line arguments to the orchestration in `opentf-release-core`. There is **no
//! `--adapter` flag**: which ecosystems are active is read from `release.toml` (written by
//! `init`), the committed source of truth. `init` is interactive and creates that file.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use otf_release_adapters::generic::{GenericAdapter, GenericPkg};
use otf_release_core::adapter::Adapter;
use otf_release_core::config::{Ecosystem, ReleaseConfig, DEFAULT_VERSION_FIELD};
use otf_release_core::init::AdapterFactory;
use otf_release_core::{init, publish, upgrade, version};

mod self_update;

/// Builds the concrete ecosystem adapters from `opentf-release-adapters`. The generic adapter is
/// configured from `release.toml`'s generic `[[package]]` entries.
struct CliAdapterFactory {
    root: PathBuf,
    generic: Vec<GenericPkg>,
}

impl AdapterFactory for CliAdapterFactory {
    fn make(&self, ecosystem: Ecosystem) -> Box<dyn Adapter> {
        match ecosystem {
            Ecosystem::Npm => Box::new(otf_release_adapters::npm::NpmAdapter::new(
                self.root.clone(),
            )),
            Ecosystem::Cargo => Box::new(otf_release_adapters::cargo::CargoAdapter::new(
                self.root.clone(),
            )),
            Ecosystem::Generic => {
                Box::new(GenericAdapter::new(self.root.clone(), self.generic.clone()))
            }
        }
    }
}

/// Translate the generic `[[package]]` entries of a config into adapter inputs.
fn generic_pkgs(config: &ReleaseConfig) -> Vec<GenericPkg> {
    config
        .packages
        .iter()
        .filter(|p| p.adapter == Ecosystem::Generic)
        .filter_map(|p| {
            p.manifest.as_ref().map(|manifest| GenericPkg {
                name: p.name.clone(),
                manifest: manifest.into(),
                version_field: p
                    .version_field
                    .clone()
                    .unwrap_or_else(|| DEFAULT_VERSION_FIELD.to_string()),
                publish: p.publish.clone(),
            })
        })
        .collect()
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
    /// Upgrade configurations and the GitHub workflow to match the latest CLI version
    Upgrade {
        /// Overwrite existing files without prompting.
        #[arg(long)]
        force: bool,
    },
    /// Edit release.toml interactively.
    Config,
    /// Non-interactive, CI: automated ephemeral release via short git hashes.
    Snapshot,
    /// Update otf-release to the latest version.
    SelfUpdate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));

    match cli.command {
        // `init` is the one command that doesn't read the config — it writes it. The generic
        // adapter is never discovered here (its packages are entered interactively).
        Command::Init { force } => {
            let factory = CliAdapterFactory {
                root: root.clone(),
                generic: Vec::new(),
            };
            init::run(&factory, &root, &init::InitOptions { force })
        }
        Command::Upgrade { force } => {
            upgrade::orchestrate(
                &root,
                &upgrade::UpgradeOptions { force },
                &otf_release_core::prompt::StdinPrompt,
            )?;
            Ok(())
        }
        Command::Config => {
            otf_release_core::config_cmd::orchestrate(&root)?;
            Ok(())
        }
        Command::Snapshot => {
            let config = ReleaseConfig::load(&root)?;
            let factory = CliAdapterFactory {
                root: root.clone(),
                generic: generic_pkgs(&config),
            };
            for eco in &config.adapters {
                let adapter = factory.make(*eco);
                otf_release_core::snapshot::run(adapter.as_ref(), &root, &config)?;
            }
            Ok(())
        }
        Command::SelfUpdate => {
            self_update::run()?;
            Ok(())
        }

        // Every other command reads `release.toml` and acts on each enabled ecosystem.
        Command::Version {
            dry_run,
            first_release,
        } => {
            let config = ReleaseConfig::load(&root)?;
            let factory = CliAdapterFactory {
                root: root.clone(),
                generic: generic_pkgs(&config),
            };
            let opts = version::VersionOptions {
                dry_run,
                first_release,
                skip_pr: false,
            };
            let adapters: Vec<Box<dyn Adapter>> = config
                .adapters
                .iter()
                .map(|eco| factory.make(*eco))
                .collect();
            let adapter_refs: Vec<&dyn Adapter> =
                adapters.iter().map(|adapter| adapter.as_ref()).collect();
            version::run_many(&adapter_refs, &root, &opts, &config)?;
            Ok(())
        }

        Command::Publish {
            artifacts_dir,
            dry_run,
        } => {
            let config = ReleaseConfig::load(&root)?;
            let factory = CliAdapterFactory {
                root: root.clone(),
                generic: generic_pkgs(&config),
            };
            // build-only packages ship via the GitHub Release the workflow creates, never a
            // registry — so `publish` skips them.
            let skip = config.build_only_names();
            let adapters: Vec<Box<dyn Adapter>> = config
                .adapters
                .iter()
                .map(|eco| factory.make(*eco))
                .collect();
            let adapter_refs: Vec<&dyn Adapter> =
                adapters.iter().map(|adapter| adapter.as_ref()).collect();
            publish::run_many(
                &adapter_refs,
                &root,
                &publish::PublishOptions {
                    artifacts_dir,
                    dry_run,
                    skip,
                },
                &config.hooks,
            )?;
            Ok(())
        }
    }
}
