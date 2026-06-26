//! # opentf-release-adapters
//!
//! Registry/ecosystem adapters implementing [`otf_release_core::adapter::Adapter`].
//!
//! - [`npm::NpmAdapter`] — the npm/Node ecosystem.
//! - [`cargo::CargoAdapter`] — the Rust/crates.io ecosystem (initial implementation).
//! - [`generic::GenericAdapter`] — config-driven, for registries without native support (e.g.
//!   JSR): version from a named manifest field, an optional user-supplied publish command.
//!
//! The npm/cargo adapters share the [`command::CommandRunner`] seam so registry/publish calls
//! are testable.
#![allow(dead_code, unused_variables)]

pub mod cargo;
pub mod command;
pub mod generic;
pub mod npm;
