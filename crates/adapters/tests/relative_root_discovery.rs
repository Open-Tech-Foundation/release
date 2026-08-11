//! Discovery run from a **relative** root — the shape `otf-release` actually uses.
//!
//! The CLI defaults `--root` to `.`, and `glob` drops that leading `./` from the paths it yields.
//! Anything that compares a pattern string against an already-globbed path is therefore comparing
//! two spellings of the same directory, and silently fails to match. That is a whole-process
//! concern (it needs `set_current_dir`), so it lives in its own test binary rather than beside the
//! unit tests, where a parallel thread changing the working directory would corrupt its neighbours.

use std::fs;

use otf_release_adapters::npm::NpmAdapter;
use otf_release_core::adapter::Adapter;

#[test]
fn a_negated_pattern_excludes_a_member_under_a_relative_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("package.json"),
        r#"{"name":"root","private":true}"#,
    )
    .unwrap();
    fs::write(
        root.join("pnpm-workspace.yaml"),
        "packages:\n  - 'packages/*'\n  - '!packages/private-app'\n",
    )
    .unwrap();
    for dir in ["packages/lib", "packages/private-app"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        let name = dir.rsplit('/').next().unwrap();
        fs::write(
            root.join(dir).join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
    }

    std::env::set_current_dir(root).unwrap();
    let names: Vec<String> = NpmAdapter::new(".")
        .discover_packages()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["lib"]);
}
