//! # opentf-release-core
//!
//! Ecosystem-agnostic release orchestration. This crate knows nothing about npm,
//! cargo, or any other registry — it only talks to an [`adapter::Adapter`].
//!
//! The flow is split across modules that mirror the CLI:
//!
//! - [`adapter`]  — the [`adapter::Adapter`] trait + shared domain types ([`adapter::Pkg`], [`adapter::Bump`], …).
//! - [`config`]   — `release.toml`, the committed source of truth ([`config::ReleaseConfig`]).
//! - [`graph`]    — internal dependency graph, topological sort, and the bump cascade engine.
//! - [`changelog`]— Keep a Changelog parser/rewriter (`[Unreleased]` → dated section).
//! - [`preflight`]— strict tag/changelog compliance gate (all-or-nothing).
//! - [`summary`]  — confirmation / dry-run rendering.
//! - [`version`]  — the interactive `version` command (local; produces a release PR).
//! - [`publish`]  — the non-interactive `publish` command (CI; stateless, resumable).
//! - [`init`]     — the interactive `release.yml` generator.
//!
//! See `docs/` at the repo root for the full design.
#![allow(dead_code, unused_variables)]

pub mod adapter;
pub mod changelog;
pub mod config;
pub mod date;
pub mod discover;
pub mod forge;
pub mod git;
pub mod graph;
pub mod init;
pub mod preflight;
pub mod prompt;
pub mod publish;
pub mod summary;
pub mod version;
