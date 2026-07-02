//! Internal dependency graph: topological ordering (for publish) and the bump cascade
//! engine (for version).
//!
//! The cascade is transitive — each newly bumped dependent is re-fed into the walk — and
//! **merges** bumps (via [`crate::adapter::Bump::merge`]) when a package is reached by multiple
//! paths, so a prerelease reached alongside a stable bump keeps its prerelease intent instead of
//! being masked by a numerically-larger stable bump. It **terminates at private packages**, which
//! are graph leaves that are never versioned or published.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, bail, Context, Result};

use crate::adapter::{Adapter, Bump, DepKind, Pkg};

/// A resolved set of packages plus their internal edges, ready for sorting and cascading.
#[derive(Debug)]
pub struct Graph<'a> {
    pub packages: &'a [Pkg],
    by_name: HashMap<String, usize>,
    /// For each package index, the packages that depend on it, with the edge kind.
    /// One entry per declared edge (a dependent listing the same dep under two sections
    /// appears twice — intentional, so the cascade max-merges both rules).
    dependents: HashMap<usize, Vec<(usize, DepKind)>>,
}

impl<'a> Graph<'a> {
    pub fn build(packages: &'a [Pkg]) -> Result<Self> {
        let mut by_name = HashMap::with_capacity(packages.len());
        for (i, p) in packages.iter().enumerate() {
            if by_name.insert(p.name.clone(), i).is_some() {
                bail!("duplicate package name in workspace: {}", p.name);
            }
        }

        let mut dependents: HashMap<usize, Vec<(usize, DepKind)>> = HashMap::new();
        for (i, p) in packages.iter().enumerate() {
            for dep in &p.internal_deps {
                let j = *by_name.get(&dep.name).ok_or_else(|| {
                    anyhow!(
                        "{} depends on unknown internal package {}",
                        p.name,
                        dep.name
                    )
                })?;
                dependents.entry(j).or_default().push((i, dep.kind.clone()));
            }
        }

        Ok(Self {
            packages,
            by_name,
            dependents,
        })
    }

    /// Dependencies before dependents. Errors on cycles, naming the packages involved.
    pub fn topo_order(&self) -> Result<Vec<&'a Pkg>> {
        let n = self.packages.len();

        // Unique dependency targets per package (a dep listed in two sections is one edge here).
        let mut deps_of: Vec<HashSet<usize>> = vec![HashSet::new(); n];
        for (i, p) in self.packages.iter().enumerate() {
            for dep in &p.internal_deps {
                if let Some(&j) = self.by_name.get(&dep.name) {
                    if j != i {
                        deps_of[i].insert(j);
                    }
                }
            }
        }

        let mut in_degree: Vec<usize> = deps_of.iter().map(HashSet::len).collect();
        let mut rdeps: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, deps) in deps_of.iter().enumerate() {
            for &j in deps {
                rdeps[j].push(i);
            }
        }

        // Kahn's algorithm; seeding/visiting in index order keeps the output deterministic.
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for &k in &rdeps[i] {
                in_degree[k] -= 1;
                if in_degree[k] == 0 {
                    queue.push_back(k);
                }
            }
        }

        if order.len() != n {
            let cyclic: Vec<&str> = (0..n)
                .filter(|&i| in_degree[i] > 0)
                .map(|i| self.packages[i].name.as_str())
                .collect();
            bail!("dependency cycle among: {}", cyclic.join(", "));
        }

        Ok(order.into_iter().map(|i| &self.packages[i]).collect())
    }

    /// Given the user's explicitly selected bumps, compute the full bump map after cascading
    /// through dependents via `adapter.dependent_bump`, transitively, taking the max bump,
    /// and stopping at private leaves (which never appear in the result).
    pub fn cascade(
        &self,
        adapter: &dyn Adapter,
        selected: &HashMap<String, Bump>,
    ) -> Result<HashMap<String, Bump>> {
        let mut result: HashMap<String, Bump> = HashMap::new();
        let mut work: VecDeque<usize> = VecDeque::new();

        // Seed with the explicit selections (private packages are never versioned).
        for (name, bump) in selected {
            let idx = *self
                .by_name
                .get(name)
                .ok_or_else(|| anyhow!("selected unknown package: {name}"))?;
            if !self.packages[idx].publishable {
                continue;
            }
            if raise(&mut result, name, bump.clone())? {
                work.push_back(idx);
            }
        }

        // Propagate to dependents until no bump increases.
        while let Some(idx) = work.pop_front() {
            let src_bump = result[&self.packages[idx].name].clone();
            let Some(deps) = self.dependents.get(&idx) else {
                continue;
            };
            for (dep_idx, kind) in deps {
                let dep_pkg = &self.packages[*dep_idx];
                if !dep_pkg.publishable {
                    continue; // cascade terminates at private leaves
                }
                let bump = adapter.dependent_bump(src_bump.clone(), kind);
                if raise(&mut result, &dep_pkg.name, bump)? {
                    work.push_back(*dep_idx);
                }
            }
        }

        Ok(result)
    }
}

/// Merge `bump` into `name`'s current bump via [`Bump::merge`] (prerelease-aware, not raw enum
/// order). Returns `true` if the stored bump changed, so the caller re-propagates to dependents.
/// Errors only on an unresolvable cross-channel conflict, naming the package involved.
fn raise(result: &mut HashMap<String, Bump>, name: &str, bump: Bump) -> Result<bool> {
    match result.get(name) {
        Some(existing) => {
            let merged = existing
                .merge(&bump)
                .with_context(|| format!("merging bumps for package `{name}`"))?;
            if &merged == existing {
                Ok(false)
            } else {
                result.insert(name.to_string(), merged);
                Ok(true)
            }
        }
        None => {
            result.insert(name.to_string(), bump);
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A fake adapter using the npm cascade rule: peerDep mirrors, everything else patches.
    struct FakeAdapter;

    impl Adapter for FakeAdapter {
        fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump {
            match kind {
                DepKind::PeerDep => dep_bump,
                _ => Bump::Patch,
            }
        }
        fn discover_packages(&self) -> Result<Vec<Pkg>> {
            unreachable!()
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
        fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
    }

    fn pkg(name: &str, publishable: bool, deps: &[(&str, DepKind)]) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: PathBuf::from(format!("{name}/package.json")),
            changelog_path: PathBuf::from(format!("{name}/CHANGELOG.md")),
            publishable,
            internal_deps: deps
                .iter()
                .map(|(n, k)| crate::adapter::InternalDep {
                    name: (*n).to_string(),
                    kind: k.clone(),
                    range: "^1.0.0".to_string(),
                })
                .collect(),
        }
    }

    fn pos(order: &[&Pkg], name: &str) -> usize {
        order.iter().position(|p| p.name == name).unwrap()
    }

    #[test]
    fn topo_order_places_dependencies_first() {
        // Diamond: a -> {b, c} -> d
        let pkgs = vec![
            pkg("a", true, &[]),
            pkg("b", true, &[("a", DepKind::Dep)]),
            pkg("c", true, &[("a", DepKind::Dep)]),
            pkg("d", true, &[("b", DepKind::Dep), ("c", DepKind::Dep)]),
        ];
        let order = Graph::build(&pkgs).unwrap().topo_order().unwrap();
        assert!(pos(&order, "a") < pos(&order, "b"));
        assert!(pos(&order, "a") < pos(&order, "c"));
        assert!(pos(&order, "b") < pos(&order, "d"));
        assert!(pos(&order, "c") < pos(&order, "d"));
    }

    #[test]
    fn topo_order_errors_on_cycle() {
        let pkgs = vec![
            pkg("a", true, &[("b", DepKind::Dep)]),
            pkg("b", true, &[("a", DepKind::Dep)]),
        ];
        let err = Graph::build(&pkgs).unwrap().topo_order().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cycle"), "got: {msg}");
        assert!(msg.contains('a') && msg.contains('b'), "got: {msg}");
    }

    #[test]
    fn build_rejects_duplicate_names_and_unknown_edges() {
        let dup = vec![pkg("a", true, &[]), pkg("a", true, &[])];
        assert!(Graph::build(&dup)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        let unknown = vec![pkg("a", true, &[("ghost", DepKind::Dep)])];
        assert!(Graph::build(&unknown)
            .unwrap_err()
            .to_string()
            .contains("ghost"));
    }

    #[test]
    fn cascade_is_transitive_max_merged_and_stops_at_private() {
        let pkgs = vec![
            pkg("core", true, &[]),
            pkg("utils", true, &[("core", DepKind::Dep)]),
            pkg("sdk", true, &[("core", DepKind::PeerDep)]),
            // x reached two ways: patch via core (dep), major via sdk (peer) -> max = major
            pkg(
                "x",
                true,
                &[("core", DepKind::Dep), ("sdk", DepKind::PeerDep)],
            ),
            pkg("app", false, &[("core", DepKind::Dep)]), // private leaf
        ];
        let graph = Graph::build(&pkgs).unwrap();

        let selected = HashMap::from([("core".to_string(), Bump::Major)]);
        let result = graph.cascade(&FakeAdapter, &selected).unwrap();

        assert_eq!(result.get("core"), Some(&Bump::Major));
        assert_eq!(result.get("utils"), Some(&Bump::Patch)); // dep -> patch
        assert_eq!(result.get("sdk"), Some(&Bump::Major)); // peer -> mirror
        assert_eq!(result.get("x"), Some(&Bump::Major)); // max(patch, major)
        assert_eq!(result.get("app"), None, "private leaf is never bumped");
    }

    #[test]
    fn cascade_keeps_prerelease_when_merged_with_a_stable_path() {
        // x is reached two ways from core: a peerDep (mirrors the PreMajor beta) and a plain dep
        // (patch). The old raw-enum `max` kept Patch — a stable bump whose peer range would point
        // at `-beta`. The merge must keep the prerelease.
        let pkgs = vec![
            pkg("core", true, &[]),
            pkg(
                "x",
                true,
                &[("core", DepKind::PeerDep), ("core", DepKind::Dep)],
            ),
        ];
        let graph = Graph::build(&pkgs).unwrap();
        let selected = HashMap::from([("core".to_string(), Bump::PreMajor("beta".to_string()))]);
        let result = graph.cascade(&FakeAdapter, &selected).unwrap();

        assert_eq!(result.get("x"), Some(&Bump::PreMajor("beta".to_string())));
    }

    #[test]
    fn cascade_errors_on_conflicting_prerelease_channels() {
        // x mirrors two peers going to *different* prerelease channels — unresolvable.
        let pkgs = vec![
            pkg("beta-core", true, &[]),
            pkg("rc-core", true, &[]),
            pkg(
                "x",
                true,
                &[("beta-core", DepKind::PeerDep), ("rc-core", DepKind::PeerDep)],
            ),
        ];
        let graph = Graph::build(&pkgs).unwrap();
        let selected = HashMap::from([
            ("beta-core".to_string(), Bump::PreMajor("beta".to_string())),
            ("rc-core".to_string(), Bump::PreMajor("rc".to_string())),
        ]);
        let err = graph.cascade(&FakeAdapter, &selected).unwrap_err().to_string();
        assert!(err.contains('x'), "error should name the package: {err}");
    }

    #[test]
    fn cascade_ignores_private_selections() {
        let pkgs = vec![pkg("app", false, &[])];
        let graph = Graph::build(&pkgs).unwrap();
        let selected = HashMap::from([("app".to_string(), Bump::Major)]);
        assert!(graph.cascade(&FakeAdapter, &selected).unwrap().is_empty());
    }
}
