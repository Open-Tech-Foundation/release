//! `otf-release` — the CLI entry point.
//!
//! Wires command-line arguments to the orchestration in `opentf-release-core`, using the
//! npm adapter from `opentf-release-adapters` (the only adapter in v1).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use opentf_release_adapters::npm::NpmAdapter;
use opentf_release_core::{init, publish, version};

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
    let adapter = NpmAdapter::new(root.clone());

    match cli.command {
        Command::Version {
            dry_run,
            first_release,
        } => version::run(
            &adapter,
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
            &adapter,
            &publish::PublishOptions {
                artifacts_dir,
                dry_run,
            },
        ),
        Command::Init { force } => init::run(&adapter, &init::InitOptions { force }),
    }
}
