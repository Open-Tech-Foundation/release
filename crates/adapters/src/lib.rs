//! # opentf-release-adapters
//!
//! Registry/ecosystem adapters implementing [`opentf_release_core::adapter::Adapter`].
//!
//! - [`npm::NpmAdapter`] — the npm/Node ecosystem.
//! - [`cargo::CargoAdapter`] — the Rust/crates.io ecosystem (initial implementation).
//!
//! Both share the [`command::CommandRunner`] seam so registry/publish calls are testable.
#![allow(dead_code, unused_variables)]

pub mod cargo;
pub mod command;
pub mod npm;
