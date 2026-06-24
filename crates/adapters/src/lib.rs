//! # opentf-release-adapters
//!
//! Registry/ecosystem adapters implementing [`opentf_release_core::adapter::Adapter`].
//!
//! v1 ships exactly one: [`npm::NpmAdapter`]. The cargo, PyPI, … adapters are deferred —
//! the trait isolates them, but no implementations exist yet.
#![allow(dead_code, unused_variables)]

pub mod npm;
