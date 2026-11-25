//! The pieces of `export` and `measure` that touch files or choose items, kept
//! here so they can be tested without a database.

use std::path::Path;

use anyhow::{Context, Result, bail};
use trovato_kernel::cron::IndexDocument;

/// Every `len / n`-th of `docs`, in order: a systematic sample, so each content
/// type appears in proportion to its share.
fn every_kth<T>(docs: Vec<T>, n: usize) -> Vec<T> {
    let len = docs.len();
    let n = n.min(len);
    if n == 0 {
        return Vec::new();
    }
    let wanted: std::collections::BTreeSet<usize> = (0..n).map(|i| i * len / n).collect();
    docs.into_iter()
        .enumerate()
        .filter(|(i, _)| wanted.contains(i))
        .map(|(_, d)| d)
        .collect()
}

/// A deterministic sample of `n` (or every item, when there are fewer): about
/// a quarter drawn systematically from the items with visible comments, the
/// rest systematically from the others, both in id order. When one group
/// cannot fill its share, the other fills the remaining slots.
pub fn sample(mut docs: Vec<IndexDocument>, n: usize) -> Vec<IndexDocument> {
    docs.sort_by_key(|d| d.id);
    let n = n.min(docs.len());
    let (with, without): (Vec<_>, Vec<_>) = docs.into_iter().partition(|d| !d.comments.is_empty());
    // Commented items get a quarter, or more when the others run short.
    let from_with = (n / 4).max(n.saturating_sub(without.len())).min(with.len());
    let mut chosen = every_kth(with, from_with);
    chosen.extend(every_kth(without, n - chosen.len()));
    chosen.sort_by_key(|d| d.id);
    chosen
}

/// Create `dir`, or accept it when it exists and is empty. A directory that
/// already holds files is refused: `parity` reads every `.html` in it, so a
/// file left from an earlier export (a deleted item, one no longer public, or
/// the rest of a larger export) would be compared as if it were current.
pub fn prepare_empty_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let mut entries =
            std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
        if entries.next().is_some() {
            bail!(
                "{} is not empty; export into a new or empty directory",
                dir.display()
            );
        }
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))
}

/// Put a freshly built index at `output`, as `build_index_inner` does (old
/// index to `backup`, new one in, backup removed), but only once the new one
/// is known to be there: a build that left no `new_index` directory, or one
/// without `pagefind-entry.json`, is refused and the old index is left alone.
pub fn swap_into(new_index: &Path, output: &Path, backup: &Path) -> Result<()> {
    if !new_index.join("pagefind-entry.json").is_file() {
        bail!(
            "the build left no index at {}; keeping the one at {}",
            new_index.display(),
            output.display()
        );
    }
    if output.exists() {
        std::fs::rename(output, backup).context("move the old index aside")?;
    }
    if let Err(e) = std::fs::rename(new_index, output) {
        if backup.exists() {
            std::fs::rename(backup, output).ok();
        }
        return Err(e).context("move the new index into place");
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup).ok();
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn doc(i: u128, comments: usize) -> IndexDocument {
        IndexDocument {
            id: Uuid::from_u128(i),
            item_type: "page".into(),
            title: format!("t{i}"),
            url: format!("/item/{i}"),
            fields: Vec::new(),
            comments: vec!["c".into(); comments],
            description: String::new(),
            location: String::new(),
            event_dates: String::new(),
            created_display: String::new(),
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("b216-{name}-{}", Uuid::now_v7().simple()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn a_sample_is_full_when_only_commented_items_exist() {
        let docs: Vec<_> = (0..60).map(|i| doc(i, 1)).collect();
        assert_eq!(sample(docs.clone(), 60).len(), 60);
        assert_eq!(sample(docs, 1).len(), 1);
    }

    #[test]
    fn a_sample_is_full_when_only_uncommented_items_exist() {
        let docs: Vec<_> = (0..60).map(|i| doc(i, 0)).collect();
        assert_eq!(sample(docs, 30).len(), 30);
    }

    #[test]
    fn a_sample_draws_a_quarter_from_commented_items_and_no_duplicates() {
        let docs: Vec<_> = (0..200).map(|i| doc(i, usize::from(i % 5 == 0))).collect();
        let chosen = sample(docs, 60);
        assert_eq!(chosen.len(), 60);
        assert_eq!(chosen.iter().filter(|d| !d.comments.is_empty()).count(), 15);
        let mut ids: Vec<_> = chosen.iter().map(|d| d.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 60);
    }

    #[test]
    fn a_sample_larger_than_the_corpus_is_the_corpus() {
        let docs: Vec<_> = (0..7).map(|i| doc(i, usize::from(i < 2))).collect();
        assert_eq!(sample(docs, 60).len(), 7);
    }

    #[test]
    fn export_refuses_a_directory_that_holds_files() {
        let dir = tmp("nonempty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale.html"), "old").unwrap();
        assert!(prepare_empty_dir(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_accepts_a_new_or_empty_directory() {
        let dir = tmp("empty");
        prepare_empty_dir(&dir).unwrap();
        assert!(dir.is_dir());
        prepare_empty_dir(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_build_that_left_no_index_keeps_the_old_one() {
        let root = tmp("swap-missing");
        let output = root.join("pagefind");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("pagefind-entry.json"), "old").unwrap();
        let result = swap_into(&root.join("nothing"), &output, &root.join("backup"));
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(output.join("pagefind-entry.json")).unwrap(),
            "old"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_built_index_replaces_the_old_one() {
        let root = tmp("swap-ok");
        let output = root.join("pagefind");
        let new_index = root.join("new");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("pagefind-entry.json"), "old").unwrap();
        std::fs::create_dir_all(&new_index).unwrap();
        std::fs::write(new_index.join("pagefind-entry.json"), "new").unwrap();
        swap_into(&new_index, &output, &root.join("backup")).unwrap();
        assert_eq!(
            std::fs::read_to_string(output.join("pagefind-entry.json")).unwrap(),
            "new"
        );
        assert!(!root.join("backup").exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
