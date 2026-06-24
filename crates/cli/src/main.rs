//! `otf-release` — the CLI entry point.
//!
//! Wires command-line arguments to the orchestration in `opentf-release-core`, selecting an
//! ecosystem adapter (`npm` or `cargo`) from `opentf-release-adapters`.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use opentf_release_core::adapter::Adapter;
use opentf_release_core::{init, publish, version};

/// Which ecosystem adapter to use. A repo can have several; `init` bakes the choice into the
/// generated workflow, which passes `--adapter` explicitly.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AdapterKind {
    Npm,
    Cargo,
}

/// Curated-changelog, manual-bump release CLI for polyglot monorepos.
#[derive(Debug, Parser)]
#[command(name = "otf-release", version, about)]
struct Cli {
    /// Workspace root (defaults to the current directory).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// Ecosystem adapter to use.
    #[arg(long, global = true, value_enum, default_value = "npm")]
    adapter: AdapterKind,

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
    /// Generate a single `.github/workflows/release.yml`.
    Init {
        /// Overwrite an existing workflow without prompting.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));
    let adapter: Box<dyn Adapter> = match cli.adapter {
        AdapterKind::Npm => Box::new(opentf_release_adapters::npm::NpmAdapter::new(root.clone())),
        AdapterKind::Cargo => Box::new(opentf_release_adapters::cargo::CargoAdapter::new(
            root.clone(),
        )),
    };
    let adapter = adapter.as_ref();

    match cli.command {
        Command::Version {
            dry_run,
            first_release,
        } => version::run(
            adapter,
            &root,
            &version::VersionOptions {
                dry_run,
                first_release,
            },
        ),
        Command::Publish {
            artifacts_dir,
            dry_run,
        } => publish::run(
            adapter,
            &root,
            &publish::PublishOptions {
                artifacts_dir,
                dry_run,
            },
        ),
        Command::Init { force } => init::run(adapter, &root, &init::InitOptions { force }),
    }
}
