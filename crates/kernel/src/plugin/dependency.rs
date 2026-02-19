//! Plugin dependency resolution using topological sort.
//!
//! Ensures plugins are loaded in the correct order based on their dependencies.
//! Uses Kahn's algorithm for topological sorting with cycle detection.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Result, bail};

use super::info_parser::PluginInfo;

/// Resolve plugin load order based on dependencies.
///
/// Returns plugins sorted so that dependencies come before dependents.
/// Detects and reports dependency cycles.
///
/// # Arguments
/// * `plugins` - Map of plugin name to PluginInfo
///
/// # Returns
/// Ordered list of plugin names (load order)
///
/// # Errors
/// Returns error if:
/// - A plugin declares a dependency that doesn't exist
/// - There is a circular dependency
pub fn resolve_load_order(plugins: &HashMap<String, PluginInfo>) -> Result<Vec<String>> {
    // Build dependency graph
    // in_degree[p] = number of plugins that p depends on (that must load first)
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    // Initialize all plugins with in_degree 0
    for name in plugins.keys() {
        in_degree.insert(name, 0);
        dependents.entry(name.as_str()).or_default();
    }

    // Build the graph
    for (name, info) in plugins {
        for dep in &info.dependencies {
            // Check if dependency exists
            if !plugins.contains_key(dep) {
                bail!("plugin '{name}' depends on '{dep}' which is not installed");
            }

            // name depends on dep, so dep must load first
            // This means name has in_degree +1 from dep
            if let Some(degree) = in_degree.get_mut(name.as_str()) {
                *degree += 1;
            }
            dependents.entry(dep.as_str()).or_default().push(name);
        }
    }

    // Kahn's algorithm with deterministic ordering.
    // Sort newly-unblocked nodes so independent plugins load in alphabetical order.
    let mut result = Vec::with_capacity(plugins.len());
    let mut queue: VecDeque<&str> = VecDeque::new();

    // Seed with zero-in-degree nodes in sorted order
    let mut roots: Vec<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(name, _)| *name)
        .collect();
    roots.sort();
    queue.extend(roots);

    while let Some(plugin) = queue.pop_front() {
        result.push(plugin.to_string());

        // For each plugin that depends on this one, decrease its in_degree
        if let Some(deps) = dependents.get(plugin) {
            let mut newly_ready: Vec<&str> = Vec::new();
            for dependent in deps {
                if let Some(degree) = in_degree.get_mut(*dependent) {
                    *degree -= 1;
                    if *degree == 0 {
                        newly_ready.push(*dependent);
                    }
                }
            }
            newly_ready.sort();
            queue.extend(newly_ready);
        }
    }

    // Check for cycles
    if result.len() != plugins.len() {
        // Find the plugins involved in the cycle
        let loaded: HashSet<_> = result.iter().map(|s| s.as_str()).collect();
        let in_cycle: Vec<_> = plugins
            .keys()
            .filter(|k| !loaded.contains(k.as_str()))
            .cloned()
            .collect();

        bail!(
            "circular dependency detected involving plugins: {}",
            in_cycle.join(", ")
        );
    }

    Ok(result)
}

/// Check if a plugin's dependencies are satisfied.
pub fn check_dependencies(plugin: &PluginInfo, available: &HashSet<String>) -> Result<()> {
    for dep in &plugin.dependencies {
        if !available.contains(dep) {
            bail!(
                "plugin '{}' requires '{}' which is not available",
                plugin.name,
                dep
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::info_parser::{MigrationConfig, TapConfig};

    fn make_plugin(name: &str, deps: Vec<&str>) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            description: format!("{name} plugin"),
            version: "1.0.0".to_string(),
            default_enabled: true,
            dependencies: deps.into_iter().map(String::from).collect(),
            taps: TapConfig::default(),
            migrations: MigrationConfig::default(),
        }
    }

    #[test]
    fn no_dependencies() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![]));
        plugins.insert("b".to_string(), make_plugin("b", vec![]));
        plugins.insert("c".to_string(), make_plugin("c", vec![]));

        let order = resolve_load_order(&plugins).unwrap();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn simple_chain() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![]));
        plugins.insert("b".to_string(), make_plugin("b", vec!["a"]));
        plugins.insert("c".to_string(), make_plugin("c", vec!["b"]));

        let order = resolve_load_order(&plugins).unwrap();

        // a must come before b, b must come before c
        let a_pos = order.iter().position(|x| x == "a").unwrap();
        let b_pos = order.iter().position(|x| x == "b").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();

        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn diamond_dependency() {
        // a depends on b and c, both depend on d
        let mut plugins = HashMap::new();
        plugins.insert("d".to_string(), make_plugin("d", vec![]));
        plugins.insert("b".to_string(), make_plugin("b", vec!["d"]));
        plugins.insert("c".to_string(), make_plugin("c", vec!["d"]));
        plugins.insert("a".to_string(), make_plugin("a", vec!["b", "c"]));

        let order = resolve_load_order(&plugins).unwrap();

        let d_pos = order.iter().position(|x| x == "d").unwrap();
        let b_pos = order.iter().position(|x| x == "b").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();
        let a_pos = order.iter().position(|x| x == "a").unwrap();

        // d must come first
        assert!(d_pos < b_pos);
        assert!(d_pos < c_pos);
        // b and c must come before a
        assert!(b_pos < a_pos);
        assert!(c_pos < a_pos);
    }

    #[test]
    fn missing_dependency() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec!["missing"]));

        let result = resolve_load_order(&plugins);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn circular_dependency_direct() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec!["b"]));
        plugins.insert("b".to_string(), make_plugin("b", vec!["a"]));

        let result = resolve_load_order(&plugins);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("circular"));
    }

    #[test]
    fn circular_dependency_indirect() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec!["b"]));
        plugins.insert("b".to_string(), make_plugin("b", vec!["c"]));
        plugins.insert("c".to_string(), make_plugin("c", vec!["a"]));

        let result = resolve_load_order(&plugins);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("circular"));
    }

    #[test]
    fn deterministic_order_for_independent_plugins() {
        // Independent plugins (no deps) should always come out in sorted order.
        let mut plugins = HashMap::new();
        plugins.insert("zebra".to_string(), make_plugin("zebra", vec![]));
        plugins.insert("alpha".to_string(), make_plugin("alpha", vec![]));
        plugins.insert("middle".to_string(), make_plugin("middle", vec![]));

        // Run multiple times to confirm determinism
        for _ in 0..10 {
            let order = resolve_load_order(&plugins).unwrap();
            assert_eq!(order, vec!["alpha", "middle", "zebra"]);
        }
    }

    #[test]
    fn check_dependencies_satisfied() {
        let plugin = make_plugin("test", vec!["dep1", "dep2"]);
        let available: HashSet<_> = ["dep1", "dep2", "dep3"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        assert!(check_dependencies(&plugin, &available).is_ok());
    }

    #[test]
    fn check_dependencies_missing() {
        let plugin = make_plugin("test", vec!["dep1", "missing"]);
        let available: HashSet<_> = ["dep1"].iter().map(|s| s.to_string()).collect();

        let result = check_dependencies(&plugin, &available);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }
}
