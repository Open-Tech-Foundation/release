use std::path::Path;
use anyhow::Result;
use inquire::{Select, MultiSelect, Text};

use crate::config::{ReleaseConfig, Ecosystem, Target};

pub fn orchestrate(root: &Path) -> Result<()> {
    let mut config = ReleaseConfig::load(root)?;

    loop {
        let choice = Select::new(
            "What would you like to configure?",
            vec![
                "Lifecycle Hooks",
                "Ecosystems",
                "Build Matrix (OS & Architectures)",
                "Exit",
            ],
        )
        .prompt()?;

        match choice {
            "Lifecycle Hooks" => {
                let hook_choice = Select::new(
                    "Which hook stage?",
                    vec!["pre_version", "post_version", "pre_publish", "post_publish", "Back"],
                )
                .prompt()?;

                if hook_choice == "Back" {
                    continue;
                }

                let current = match hook_choice {
                    "pre_version" => &config.hooks.pre_version,
                    "post_version" => &config.hooks.post_version,
                    "pre_publish" => &config.hooks.pre_publish,
                    "post_publish" => &config.hooks.post_publish,
                    _ => unreachable!(),
                };

                let current_str = current.join(", ");
                let new_hook_str = Text::new(&format!("Commands for {} (comma-separated):", hook_choice))
                    .with_initial_value(&current_str)
                    .prompt()?;

                let new_hooks: Vec<String> = new_hook_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                match hook_choice {
                    "pre_version" => config.hooks.pre_version = new_hooks,
                    "post_version" => config.hooks.post_version = new_hooks,
                    "pre_publish" => config.hooks.pre_publish = new_hooks,
                    "post_publish" => config.hooks.post_publish = new_hooks,
                    _ => unreachable!(),
                }
                config.save(root)?;
                println!("Saved.");
            }
            "Ecosystems" => {
                let all_labels = Ecosystem::ALL.map(|e| e.label()).to_vec();

                let current_defaults: Vec<usize> = config
                    .adapters
                    .iter()
                    .map(|a| Ecosystem::ALL.iter().position(|e| e == a).unwrap())
                    .collect();

                let chosen_labels = MultiSelect::new("Enabled Ecosystems:", all_labels)
                    .with_default(&current_defaults)
                    .prompt()?;

                let mut new_adapters = Vec::new();
                for eco in Ecosystem::ALL {
                    if chosen_labels.contains(&eco.label()) {
                        new_adapters.push(eco);
                    }
                }
                config.adapters = new_adapters;
                config.save(root)?;
                println!("Saved.");
            }
            "Build Matrix (OS & Architectures)" => {
                let mut pkgs: Vec<String> = config.packages.iter().map(|p| p.name.clone()).collect();
                if pkgs.is_empty() {
                    println!("No configured packages in release.toml to edit matrix for.");
                    continue;
                }
                pkgs.push("Back".to_string());
                
                let pkg_choice = Select::new("Which package?", pkgs).prompt()?;
                if pkg_choice == "Back" { continue; }

                if let Some(pkg) = config.packages.iter_mut().find(|p| p.name == pkg_choice) {
                    let current_str = pkg.targets.iter().map(|t| format!("{}-{}", t.name, t.arch)).collect::<Vec<_>>().join(", ");
                    let new_targets_str = Text::new("Targets (e.g. linux-x86_64, macos-aarch64):")
                        .with_initial_value(&current_str)
                        .prompt()?;

                    let new_targets: Vec<Target> = new_targets_str
                        .split(',')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .filter_map(|s| {
                            let parts: Vec<&str> = s.split('-').collect();
                            if parts.len() == 2 {
                                Some(Target {
                                    name: parts[0].to_string(),
                                    arch: parts[1].to_string(),
                                })
                            } else {
                                println!("Warning: skipping invalid target '{}'", s);
                                None
                            }
                        })
                        .collect();

                    pkg.targets = new_targets;
                    pkg.matrix = !pkg.targets.is_empty();
                    config.save(root)?;
                    println!("Saved.");
                }
            }
            "Exit" => break,
            _ => unreachable!(),
        }
    }

    Ok(())
}
