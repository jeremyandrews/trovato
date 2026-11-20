//! A copy of a fixture plugin with its manifest edited, for a test that needs
//! the same wasm under a different `[capabilities]` table.
//!
//! Plugin directories are a search path and a later directory wins a name
//! collision, so a `TestApp` whose `plugins_dirs` ends with the copy's parent
//! loads the copy in place of the original and every other plugin as usual.

use std::path::{Path, PathBuf};

/// Copy `plugins/<name>` into a fresh temporary directory, apply `edit` to its
/// `<name>.info.toml`, and return the directory to append to `plugins_dirs`.
///
/// # Panics
///
/// Panics if the fixture is missing or a copy fails; a test cannot proceed
/// without its fixture.
pub fn copy_with_manifest(name: &str, edit: impl FnOnce(&str) -> String) -> PathBuf {
    let parent = std::env::temp_dir().join(format!(
        "trovato_plugin_copy_{}",
        uuid::Uuid::now_v7().simple()
    ));
    let source = super::project_root().join("plugins").join(name);
    let target = parent.join(name);
    copy_dir(&source, &target);
    let manifest = target.join(format!("{name}.info.toml"));
    let original = std::fs::read_to_string(&manifest).expect("read the copied manifest");
    std::fs::write(&manifest, edit(&original)).expect("write the edited manifest");
    parent
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy directory");
    for entry in std::fs::read_dir(from).expect("read the fixture directory") {
        let entry = entry.expect("a directory entry");
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).expect("copy a fixture file");
        }
    }
}
