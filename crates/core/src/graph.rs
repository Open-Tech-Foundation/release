//! Internal dependency graph: topological ordering (for publish) and the bump cascade
//! engine (for version).
//!
//! The cascade is transitive — each newly bumped dependent is re-fed into the walk — and
//! takes the **max** bump when a package is reached by multiple paths. It **terminates at
//! private packages**, which are graph leaves that are never versioned or published.

use std::collections::HashMap;

use anyhow::Result;

use crate::adapter::{Adapter, Bump, Pkg};

/// A resolved set of packages plus their internal edges, ready for sorting and cascading.
pub struct Graph<'a> {
    pub packages: &'a [Pkg],
    by_name: HashMap<String, usize>,
}

impl<'a> Graph<'a> {
    pub fn build(packages: &'a [Pkg]) -> Result<Self> {
        todo!("index packages by name and validate internal edges")
    }

    /// Dependencies before dependents. Errors on cycles.
    pub fn topo_order(&self) -> Result<Vec<&'a Pkg>> {
        todo!("Kahn / DFS topological sort, error on cycle")
    }

    /// Given the user's explicitly selected bumps, compute the full bump map after cascading
    /// through dependents via `adapter.dependent_bump`, transitively, taking the max bump,
    /// and stopping at private leaves.
    pub fn cascade(
        &self,
        adapter: &dyn Adapter,
        selected: &HashMap<String, Bump>,
    ) -> Result<HashMap<String, Bump>> {
        todo!("BFS/worklist cascade with max-bump merge, terminating at private packages")
    }
}
