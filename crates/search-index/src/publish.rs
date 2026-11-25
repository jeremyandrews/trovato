//! Putting a built bundle on disk and publishing it into the live directory.
//!
//! The write order is `docs/design/incremental-search-index.md`'s:
//!
//! 1. Refuse unless the staged bundle has an entry file and every meta file
//!    that entry names. Until this passes, nothing in the live directory is
//!    touched; a build that wrote nothing leaves the published index as it was.
//! 2. The fragments and word chunks, each moved in under its content hash name.
//!    A file already there under that name with the same bytes is kept; one
//!    with different bytes (two contents sharing a short hash) refuses the
//!    whole publish before anything moves, see [`conflicting_names`].
//! 3. The meta files, the same way.
//! 4. The fixed-name runtime files, each written to a temporary name and
//!    renamed over the old one.
//! 5. `pagefind-entry.json`, last, by rename: the single atomic pointer flip.
//!    Before it, readers see the old meta, whose files are all still present;
//!    after it, the new one, whose files were all written first.
//!
//! Nothing is deleted here. Files the new entry no longer reaches are
//! reported by [`unreferenced_files`] and removed by age by the caller, which
//! records when each stopped being referenced.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::bundle::{Bundle, ENTRY_FILE};

/// What a publish put in place.
#[derive(Debug, Clone, Default)]
pub struct Published {
    /// Every content addressed file the new entry reaches, by bundle path.
    pub referenced: BTreeSet<String>,
    /// How many of them were new to the live directory.
    pub written: usize,
    /// How many were already there under the same name.
    pub kept: usize,
}

/// Whether a bundle path is content addressed: named by its own hash, so
/// never overwritten, and removed only by age once unreferenced.
pub fn is_content_addressed(path: &str) -> bool {
    (path.starts_with("index/") && path.ends_with(".pf_index"))
        || (path.starts_with("fragment/") && path.ends_with(".pf_fragment"))
        || (path.starts_with("filter/") && path.ends_with(".pf_filter"))
        || (!path.contains('/') && path.starts_with("pagefind.") && path.ends_with(".pf_meta"))
}

/// A directory beside `live` (same parent, so renames between the two stay on
/// one filesystem) with a fresh name: `.<name>_build_<suffix>`.
pub fn staging_dir_for(live: &Path, suffix: &str) -> PathBuf {
    let parent = match live.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = live
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pagefind".to_string());
    parent.join(format!(".{name}_build_{suffix}"))
}

/// Write a bundle into `dir`, which must not exist yet.
pub fn write_bundle(bundle: &Bundle, dir: &Path) -> Result<()> {
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    for (path, bytes) in bundle {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&file, bytes).with_context(|| format!("write {}", file.display()))?;
    }
    Ok(())
}

/// The meta files a staged entry names, checked present. Errors when the entry
/// is missing, unreadable, names no language, or names a missing meta file.
fn staged_meta_files(staged: &Path) -> Result<Vec<String>> {
    let entry_path = staged.join(ENTRY_FILE);
    let bytes = std::fs::read(&entry_path).map_err(|e| {
        anyhow!("the new index has no {ENTRY_FILE} ({e}); the published index is left as it was")
    })?;
    let entry: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse the new pagefind-entry.json")?;
    let languages = entry
        .get("languages")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("the new pagefind-entry.json names no languages"))?;
    let mut metas = Vec::new();
    for (language, value) in languages {
        let hash = value
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("language {language} has no hash"))?;
        let meta = format!("pagefind.{hash}.pf_meta");
        if !staged.join(&meta).is_file() {
            bail!("the new pagefind-entry.json names {meta}, which was not written");
        }
        metas.push(meta);
    }
    Ok(metas)
}

/// Move `from` to `to`, replacing `to`. A rename when both are on one
/// filesystem; otherwise a copy to a temporary name beside `to`, then a rename.
fn move_into_place(from: &Path, to: &Path, tmp_suffix: &str) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let file_name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = to.with_file_name(format!(".{file_name}.{tmp_suffix}.tmp"));
    std::fs::copy(from, &tmp)
        .with_context(|| format!("copy {} to {}", from.display(), tmp.display()))?;
    std::fs::rename(&tmp, to)
        .with_context(|| format!("rename {} to {}", tmp.display(), to.display()))?;
    Ok(())
}

/// Publish the bundle staged in `staged` into `live`, in the write order
/// above. `tmp_suffix` names the temporary files (a fresh id per build).
///
/// Fails before touching `live` when the staged bundle has no entry file or
/// is missing a meta file its entry names.
pub fn publish(staged: &Path, live: &Path, tmp_suffix: &str) -> Result<Published> {
    let metas = staged_meta_files(staged)?;

    let staged_bundle = crate::decode::read_bundle(staged)?;
    let clashes = conflicting_names(&staged_bundle, live)?;
    if !clashes.is_empty() {
        bail!(
            "the new index reuses the name of a different file already published ({}); \
             build it against the live directory so those names are avoided",
            clashes.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    let mut published = Published::default();
    let mut runtime = Vec::new();
    let mut data_files = Vec::new();
    for path in staged_bundle.keys() {
        if path == ENTRY_FILE {
            continue;
        }
        if is_content_addressed(path) {
            if !metas.contains(path) {
                data_files.push(path.clone());
            }
        } else {
            runtime.push(path.clone());
        }
    }

    std::fs::create_dir_all(live).with_context(|| format!("create {}", live.display()))?;
    // Fragments and chunks, then the meta files.
    for path in data_files.iter().chain(metas.iter()) {
        let to = live.join(path);
        published.referenced.insert(path.clone());
        if to.is_file() {
            published.kept += 1;
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        move_into_place(&staged.join(path), &to, tmp_suffix)?;
        published.written += 1;
    }
    // The fixed-name runtime files.
    for path in &runtime {
        let to = live.join(path);
        if std::fs::read(&to).ok().as_deref() == staged_bundle.get(path).map(Vec::as_slice) {
            continue;
        }
        move_into_place(&staged.join(path), &to, tmp_suffix)?;
    }
    // The flip.
    move_into_place(&staged.join(ENTRY_FILE), &live.join(ENTRY_FILE), tmp_suffix)?;
    Ok(published)
}

/// The content addressed paths of `bundle` that `live` already holds with
/// different bytes: a short hash name shared by two contents. Packing is
/// deterministic, so the same content always has the same bytes, and a file
/// with the same name and bytes is the same file.
pub fn conflicting_names(bundle: &Bundle, live: &Path) -> Result<BTreeSet<String>> {
    let mut clashes = BTreeSet::new();
    for (path, bytes) in bundle {
        if !is_content_addressed(path) {
            continue;
        }
        match std::fs::read(live.join(path)) {
            Ok(existing) if existing != *bytes => {
                clashes.insert(path.clone());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("read {}", live.join(path).display()));
            }
        }
    }
    Ok(clashes)
}

/// Every content addressed file in `live` that `referenced` does not name, by
/// bundle path, sorted.
pub fn unreferenced_files(live: &Path, referenced: &BTreeSet<String>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(live) else {
        return Ok(out);
    };
    let mut names: Vec<String> = Vec::new();
    for entry in read {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            if matches!(name.as_str(), "index" | "fragment" | "filter") {
                for inner in std::fs::read_dir(entry.path())? {
                    let inner = inner?;
                    if inner.file_type()?.is_file() {
                        names.push(format!("{name}/{}", inner.file_name().to_string_lossy()));
                    }
                }
            }
        } else {
            names.push(name);
        }
    }
    for name in names {
        if is_content_addressed(&name) && !referenced.contains(&name) {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "b217-publish-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        dir
    }

    fn live_with_an_index(root: &Path) -> PathBuf {
        let live = root.join("pagefind");
        std::fs::create_dir_all(live.join("index")).unwrap_or_default();
        std::fs::write(live.join(ENTRY_FILE), b"{\"old\":true}").unwrap_or_default();
        std::fs::write(live.join("pagefind.en_old.pf_meta"), b"old meta").unwrap_or_default();
        std::fs::write(live.join("index/en_old.pf_index"), b"old chunk").unwrap_or_default();
        live
    }

    #[test]
    fn a_build_that_wrote_nothing_leaves_the_published_index_alone() {
        let root = tmp("nothing");
        let live = live_with_an_index(&root);
        let before = crate::decode::read_bundle(&live).unwrap_or_default();

        // A staged directory that exists but holds nothing, as a build that
        // exited cleanly without writing would leave it.
        let staged = root.join(".pagefind_build_x");
        std::fs::create_dir_all(&staged).unwrap_or_default();
        let err = publish(&staged, &live, "x").err().map(|e| e.to_string());
        assert!(
            err.as_deref()
                .is_some_and(|e| e.contains("has no pagefind-entry.json")),
            "{err:?}"
        );
        // No staged directory at all.
        assert!(publish(&root.join("missing"), &live, "x").is_err());
        // An entry naming a meta file that was never written.
        std::fs::write(
            staged.join(ENTRY_FILE),
            b"{\"version\":\"1.3.0\",\"languages\":{\"en\":{\"hash\":\"en_new\",\"wasm\":\"en\",\"page_count\":1}}}",
        )
        .unwrap_or_default();
        assert!(publish(&staged, &live, "x").is_err());

        assert_eq!(
            crate::decode::read_bundle(&live).unwrap_or_default(),
            before
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn publishing_flips_the_entry_last_and_keeps_old_files() {
        let root = tmp("flip");
        let live = live_with_an_index(&root);
        let staged = root.join(".pagefind_build_y");
        let mut bundle = Bundle::new();
        bundle.insert(
            ENTRY_FILE.into(),
            b"{\"version\":\"1.3.0\",\"languages\":{\"en\":{\"hash\":\"en_new\",\"wasm\":\"en\",\"page_count\":1}}}".to_vec(),
        );
        bundle.insert("pagefind.en_new.pf_meta".into(), b"new meta".to_vec());
        bundle.insert("index/en_new.pf_index".into(), b"new chunk".to_vec());
        bundle.insert("fragment/en_a.pf_fragment".into(), b"frag".to_vec());
        bundle.insert("pagefind.js".into(), b"js".to_vec());
        write_bundle(&bundle, &staged).unwrap_or_default();

        let Ok(published) = publish(&staged, &live, "y") else {
            panic!("publish");
        };
        assert_eq!(published.written, 3);
        let now = crate::decode::read_bundle(&live).unwrap_or_default();
        assert_eq!(now[ENTRY_FILE], bundle[ENTRY_FILE]);
        assert_eq!(now["pagefind.js"], b"js".to_vec());
        // The old files are still there for readers holding the old meta.
        assert_eq!(now["pagefind.en_old.pf_meta"], b"old meta".to_vec());
        assert_eq!(now["index/en_old.pf_index"], b"old chunk".to_vec());
        assert_eq!(
            unreferenced_files(&live, &published.referenced).unwrap_or_default(),
            vec![
                "index/en_old.pf_index".to_string(),
                "pagefind.en_old.pf_meta".to_string()
            ]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_staging_directory_sits_beside_the_live_one() {
        let dir = staging_dir_for(Path::new("/shared/pagefind"), "abc");
        assert_eq!(dir, PathBuf::from("/shared/.pagefind_build_abc"));
        let dir = staging_dir_for(Path::new("index"), "abc");
        assert_eq!(dir, PathBuf::from("./.index_build_abc"));
    }
}
