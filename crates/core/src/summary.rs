//! Confirmation / dry-run rendering shown before any files are written.
//!
//! Renders the three blocks from `docs/commands/version.md`: packages to publish
//! (selected), auto-bumped dependents, and internal range updates (including private
//! apps, which are flagged "range updated, NOT published").

/// A version change for one publishable package.
#[derive(Debug, Clone)]
pub struct VersionChange {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
    /// True when the user explicitly selected this package; false when reached via cascade.
    pub selected: bool,
    /// The parenthetical reason, e.g. `major, selected` or `patch — depends on @x/core`.
    pub note: String,
}

/// An internal dependency-range update on one consumer.
#[derive(Debug, Clone)]
pub struct RangeUpdate {
    pub consumer: String,
    pub dep: String,
    pub old_range: String,
    pub new_range: String,
    /// True for private apps: the range is updated but the package is never published.
    pub consumer_private: bool,
}

/// The full plan for a `version` run.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub changes: Vec<VersionChange>,
    pub range_updates: Vec<RangeUpdate>,
}

/// Render the human-readable confirmation summary (no trailing prompt).
pub fn render(plan: &Plan) -> String {
    let mut out = String::new();

    if !plan.changes.is_empty() {
        let name_w = plan
            .changes
            .iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(7)
            .max(7);
        let old_w = plan
            .changes
            .iter()
            .map(|c| c.old_version.len())
            .max()
            .unwrap_or(3)
            .max(3);
        let new_w = plan
            .changes
            .iter()
            .map(|c| c.new_version.len())
            .max()
            .unwrap_or(3)
            .max(3);

        out.push_str("\nVersion Bumps (Direct & Indirect):\n");
        out.push_str(&format!(
            "  {:<name_w$} | {:<old_w$} | {:<new_w$} | {}\n",
            "Package",
            "Old",
            "New",
            "Reason",
            name_w = name_w,
            old_w = old_w,
            new_w = new_w
        ));
        out.push_str(&format!(
            "  {:-<name_w$}-+-{:-<old_w$}-+-{:-<new_w$}-+--------------------------------\n",
            "",
            "",
            "",
            name_w = name_w,
            old_w = old_w,
            new_w = new_w
        ));

        for c in &plan.changes {
            out.push_str(&format!(
                "  {:<name_w$} | {:<old_w$} | {:<new_w$} | {}\n",
                c.name,
                c.old_version,
                c.new_version,
                c.note,
                name_w = name_w,
                old_w = old_w,
                new_w = new_w
            ));
        }
        out.push('\n');
    }

    if !plan.range_updates.is_empty() {
        let cons_w = plan
            .range_updates
            .iter()
            .map(|c| c.consumer.len())
            .max()
            .unwrap_or(8)
            .max(8);
        let dep_w = plan
            .range_updates
            .iter()
            .map(|c| c.dep.len())
            .max()
            .unwrap_or(10)
            .max(10);
        let old_w = plan
            .range_updates
            .iter()
            .map(|c| c.old_range.len())
            .max()
            .unwrap_or(3)
            .max(3);
        let new_w = plan
            .range_updates
            .iter()
            .map(|c| c.new_range.len())
            .max()
            .unwrap_or(3)
            .max(3);

        out.push_str("Internal Range Updates:\n");
        out.push_str(&format!(
            "  {:<cons_w$} | {:<dep_w$} | {:<old_w$} | {:<new_w$} | {}\n",
            "Consumer",
            "Dependency",
            "Old",
            "New",
            "Notes",
            cons_w = cons_w,
            dep_w = dep_w,
            old_w = old_w,
            new_w = new_w
        ));
        out.push_str(&format!(
            "  {:-<cons_w$}-+-{:-<dep_w$}-+-{:-<old_w$}-+-{:-<new_w$}-+------------------------\n",
            "",
            "",
            "",
            "",
            cons_w = cons_w,
            dep_w = dep_w,
            old_w = old_w,
            new_w = new_w
        ));

        for r in &plan.range_updates {
            let note = if r.consumer_private {
                "private app (not published)"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {:<cons_w$} | {:<dep_w$} | {:<old_w$} | {:<new_w$} | {}\n",
                r.consumer,
                r.dep,
                r.old_range,
                r.new_range,
                note,
                cons_w = cons_w,
                dep_w = dep_w,
                old_w = old_w,
                new_w = new_w
            ));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_three_blocks() {
        let plan = Plan {
            changes: vec![
                VersionChange {
                    name: "@opentf/core".into(),
                    old_version: "1.2.0".into(),
                    new_version: "2.0.0".into(),
                    selected: true,
                    note: "major, selected".into(),
                },
                VersionChange {
                    name: "@opentf/sdk".into(),
                    old_version: "1.0.0".into(),
                    new_version: "2.0.0".into(),
                    selected: false,
                    note: "mirror major — peerDep on @opentf/core".into(),
                },
            ],
            range_updates: vec![RangeUpdate {
                consumer: "playground".into(),
                dep: "@opentf/core".into(),
                old_range: "^1.2.0".into(),
                new_range: "^2.0.0".into(),
                consumer_private: true,
            }],
        };

        let out = render(&plan);
        assert!(out.contains("Version Bumps (Direct & Indirect):"));
        assert!(out.contains("@opentf/core | 1.2.0 | 2.0.0 | major, selected"));
        assert!(out.contains("mirror major — peerDep on @opentf/core"));
        assert!(out.contains("Internal Range Updates:"));
        assert!(out.contains("private app (not published)"));
    }

    #[test]
    fn omits_empty_blocks() {
        let plan = Plan {
            changes: vec![VersionChange {
                name: "a".into(),
                old_version: "1.0.0".into(),
                new_version: "1.0.1".into(),
                selected: true,
                note: "patch, selected".into(),
            }],
            range_updates: vec![],
        };
        let out = render(&plan);
        assert!(out.contains("Version Bumps (Direct & Indirect):"));
        assert!(!out.contains("Internal Range Updates:"));
    }
}
