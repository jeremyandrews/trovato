#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The incremental updater against the full build, without a database.
//!
//! A simulated ledger (page slots, a free list taken lowest first) drives a
//! random sequence of upserts, edits and deletes through
//! [`update_language`]; after every batch the published index must decode
//! equal, page number for page number, to [`build_language`] over the same
//! slots: the same fragment and word count in every slot, and every word
//! posting to the same pages with the same locations. Chunk boundaries may
//! differ (the updater keeps the frozen plan), so the bytes may too; the
//! kernel's gate 1 adds the byte comparison after compaction.
//!
//! Long pages push one chunk past 40,000 locations so it splits, and pages
//! that are deleted empty chunks out. New words land in gaps between chunks.
//!
//! The reader test publishes an update with a write refused at every point in
//! turn, and checks after each that the live directory's entry still leads
//! only to files that exist; then does the same with a real child process
//! killed at random moments mid publish.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use trovato_search_index::assemble::Slot;
use trovato_search_index::bundle::{ENTRY_FILE, entry_json_for};
use trovato_search_index::decode::{
    Bundle, decode_bundle, published_files, read_bundle, read_published,
};
use trovato_search_index::publish::{publish, publish_with, staging_dir_for, write_bundle};
use trovato_search_index::update::{PageChange, read_meta, update_language};
use trovato_search_index::{ChunkPlan, PageTerms, build_bundle, tokenize};

/// xorshift64*, so the sequence is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn tmp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "b218-incremental-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const SYLLABLES: &[&str] = &[
    "ka", "lo", "mi", "nu", "pe", "ra", "si", "to", "va", "ze", "bri", "cho", "dal", "fen", "gor",
    "hul", "jix", "kwa", "lum", "mor",
];

/// A word from the shared vocabulary, or, with `fresh`, one nobody used
/// before (it lands in a gap or past the ends).
fn word(rng: &mut Rng, fresh: Option<u64>) -> String {
    let mut w = String::new();
    for _ in 0..(2 + rng.below(2)) {
        w.push_str(SYLLABLES[rng.below(SYLLABLES.len())]);
    }
    if let Some(n) = fresh {
        w.push_str(&format!("q{n}"));
    }
    w
}

/// One page's HTML: a title and a body of `words` words.
fn page_html(rng: &mut Rng, words: usize, fresh: &mut u64) -> String {
    let mut body = String::new();
    for i in 0..words {
        if i > 0 {
            body.push(' ');
        }
        let f = if rng.below(20) == 0 {
            *fresh += 1;
            Some(*fresh)
        } else {
            None
        };
        body.push_str(&word(rng, f));
    }
    let title = word(rng, None);
    format!(
        "<html lang=\"en\"><head><title>{title}</title></head><body><div data-pagefind-body>\
         <h1 data-pagefind-weight=\"10.0\">{title}</h1><p>{body}</p></div></body></html>"
    )
}

/// A long page of one vocabulary corner, to grow one chunk past the split.
fn dense_html(n: usize) -> String {
    let mut body = String::new();
    for i in 0..3000 {
        body.push_str(&format!(" zzdense{} zzdense{}", i % 50, (i * 7 + n) % 50));
    }
    format!(
        "<html lang=\"en\"><head><title>dense {n}</title></head><body><div data-pagefind-body>\
         <p>{body}</p></div></body></html>"
    )
}

/// The simulated ledger and the pages it holds.
#[derive(Default)]
struct State {
    /// Slot -> item.
    slots: Vec<Option<u64>>,
    /// Item -> its terms.
    terms: BTreeMap<u64, PageTerms>,
    /// The chunk plan of the published index.
    plan: Vec<ChunkPlan>,
    /// The published meta stem.
    meta_hash: String,
}

impl State {
    fn slot_of(&self, item: u64) -> Option<usize> {
        self.slots.iter().position(|s| *s == Some(item))
    }

    fn full_slots(&self) -> Vec<Slot<'_>> {
        self.slots
            .iter()
            .map(|s| match s.and_then(|id| self.terms.get(&id)) {
                Some(page) => Slot::Page(page),
                None => Slot::Tombstone,
            })
            .collect()
    }
}

/// Publish `files` and the entry for one language, through a staging
/// directory, as the kernel does.
fn publish_update(live: &Path, files: &BTreeMap<String, Vec<u8>>, meta_hash: &str, pages: usize) {
    let mut bundle = Bundle::new();
    for (path, bytes) in files {
        bundle.insert(path.clone(), bytes.clone());
    }
    let meta = format!("pagefind.{meta_hash}.pf_meta");
    if !bundle.contains_key(&meta) {
        bundle.insert(meta.clone(), std::fs::read(live.join(&meta)).unwrap());
    }
    bundle.insert(
        ENTRY_FILE.into(),
        entry_json_for(&[("en", meta_hash, pages)])
            .unwrap()
            .into_bytes(),
    );
    let suffix = format!("{}", std::process::id());
    let staged = staging_dir_for(live, &suffix);
    write_bundle(&bundle, &staged).unwrap();
    publish(&staged, live, &suffix).unwrap();
    std::fs::remove_dir_all(&staged).ok();
}

/// Decoded comparison of the live index with a full build of the same slots.
fn assert_decoded_equal(live: &Path, state: &State, step: usize) {
    let ours = decode_bundle(&read_published(live).unwrap()).unwrap();
    let (_, full) =
        build_bundle(&BTreeMap::from([("en".to_string(), state.full_slots())])).unwrap();
    let theirs = decode_bundle(&full).unwrap();
    assert_eq!(ours.len(), 1);
    let (a, b) = (&ours[0], &theirs[0]);
    assert_eq!(a.meta.pages.len(), b.meta.pages.len(), "step {step}: slots");
    for (n, (pa, pb)) in a.meta.pages.iter().zip(&b.meta.pages).enumerate() {
        assert_eq!(
            pa.word_count, pb.word_count,
            "step {step}: page {n} word count"
        );
        assert_eq!(
            a.fragments[&pa.hash], b.fragments[&pb.hash],
            "step {step}: page {n} fragment"
        );
    }
    let postings = |index: &trovato_search_index::decode::DecodedIndex| {
        let mut out: BTreeMap<String, Vec<(u64, Vec<i32>)>> = BTreeMap::new();
        for chunk in index.chunks.values() {
            for w in &chunk.words {
                assert!(
                    out.insert(
                        w.word.clone(),
                        w.pages
                            .iter()
                            .map(|p| (p.page_number, p.locs.clone()))
                            .collect()
                    )
                    .is_none(),
                    "{} is in two chunks",
                    w.word
                );
            }
        }
        out
    };
    assert!(
        postings(a) == postings(b),
        "step {step}: the postings differ from a full build"
    );
    // Chunks are disjoint and in order, and each word is in the chunk the
    // meta's boundaries send it to.
    let plan_words: Vec<(String, String)> = state
        .plan
        .iter()
        .map(|c| (c.first.clone(), c.last.clone()))
        .collect();
    assert!(
        plan_words.windows(2).all(|w| w[0].1 < w[1].0),
        "step {step}: chunks overlap: {plan_words:?}"
    );
    for (i, chunk) in a.meta.index_chunks.iter().enumerate() {
        let words = &a.chunks[&chunk.hash].words;
        assert!(
            !words.is_empty(),
            "step {step}: an empty chunk is published"
        );
        assert_eq!(words.first().unwrap().word, state.plan[i].first);
        assert_eq!(words.last().unwrap().word, state.plan[i].last);
        assert!(chunk.from.as_str() <= words.first().unwrap().word.as_str());
    }
}

fn tokenized(id: u64, html: &str) -> PageTerms {
    tokenize(&format!("/{id}.html"), html).unwrap().unwrap()
}

#[test]
fn random_updates_decode_equal_to_a_full_build() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let root = tmp("random");
    let live = root.join("pagefind");
    let mut state = State::default();
    let mut fresh = 0u64;
    let mut next_id = 0u64;

    // Start from a full build of 120 pages.
    for _ in 0..120 {
        let html = {
            let words = 20 + rng.below(200);
            page_html(&mut rng, words, &mut fresh)
        };
        state.terms.insert(next_id, tokenized(next_id, &html));
        state.slots.push(Some(next_id));
        next_id += 1;
    }
    let (indexes, bundle) =
        build_bundle(&BTreeMap::from([("en".to_string(), state.full_slots())])).unwrap();
    write_bundle(&bundle, &live).unwrap();
    state.plan = indexes[0].plan.clone();
    state.meta_hash = indexes[0].meta_hash.clone();
    assert_decoded_equal(&live, &state, 0);

    let mut splits = 0;
    let mut drops = 0;
    let mut rewrites = Vec::new();
    for step in 1..=150 {
        // A batch of 1 to 6 changes to distinct items.
        let mut batch: BTreeMap<u64, Option<String>> = BTreeMap::new();
        for _ in 0..(1 + rng.below(6)) {
            let live_items: Vec<u64> = state.terms.keys().copied().collect();
            match rng.below(10) {
                // New item.
                0..=2 => {
                    let html = {
                        let words = 10 + rng.below(150);
                        page_html(&mut rng, words, &mut fresh)
                    };
                    batch.insert(next_id, Some(html));
                    next_id += 1;
                }
                // Delete.
                3..=4 if !live_items.is_empty() => {
                    batch.insert(live_items[rng.below(live_items.len())], None);
                }
                // Edit.
                _ if !live_items.is_empty() => {
                    let html = {
                        let words = 10 + rng.below(150);
                        page_html(&mut rng, words, &mut fresh)
                    };
                    batch.insert(live_items[rng.below(live_items.len())], Some(html));
                }
                _ => {}
            }
        }
        // Long pages of one corner of the vocabulary, to grow one chunk past
        // the split.
        if step % 20 == 0 && step <= 80 {
            for k in 0..3 {
                batch.insert(next_id, Some(dense_html(step + k)));
                next_id += 1;
            }
        }
        // Every dense page goes again at step 120, emptying its chunk's
        // corner of the vocabulary.
        if step == 120 {
            for (id, terms) in &state.terms {
                if terms.fragment.contains("dense") {
                    batch.insert(*id, None);
                }
            }
        }

        let (changes_owned, before_terms) = apply_batch(&mut state, &batch);
        let changes: Vec<PageChange<'_>> = changes_owned
            .iter()
            .map(|(slot, old, new)| PageChange {
                page_number: *slot,
                old: old.and_then(|id| before_terms.get(&id)).map(|t| &t.words),
                new: new.and_then(|id| state.terms.get(&id)),
            })
            .collect();
        let meta = read_meta(&live, &state.meta_hash).unwrap();
        let update = update_language(&live, "en", &meta, &state.plan, &changes)
            .unwrap_or_else(|e| panic!("step {step}: {e:#}"));
        publish_update(&live, &update.files, &update.meta_hash, update.page_count);
        splits += update.chunks_split;
        drops += update.chunks_dropped;
        rewrites.push(update.files.len());
        state.plan = update.plan;
        state.meta_hash = update.meta_hash;
        assert_decoded_equal(&live, &state, step);
    }
    assert!(splits > 0, "the sequence must split a chunk");
    assert!(drops > 0, "the sequence must empty a chunk");
    let chunks = state.plan.len();
    println!(
        "150 batches: {splits} splits, {drops} drops, {chunks} chunks at the end, \
         files written per batch: max {}, mean {:.1}",
        rewrites.iter().max().unwrap(),
        rewrites.iter().sum::<usize>() as f64 / rewrites.len() as f64
    );
    std::fs::remove_dir_all(&root).ok();
}

/// One touched slot: its number, its occupant before the batch and after.
type SlotChange = (u32, Option<u64>, Option<u64>);

/// Apply a batch (item -> new HTML, or `None` to delete) to the simulated
/// ledger as the kernel does: free the deleted items' slots, then place every
/// new item in the lowest free slot, else a new one at the end. Returns every
/// touched slot and the touched items' terms from before the batch.
fn apply_batch(
    state: &mut State,
    batch: &BTreeMap<u64, Option<String>>,
) -> (Vec<SlotChange>, BTreeMap<u64, PageTerms>) {
    let before_slots = state.slots.clone();
    let mut before_terms = BTreeMap::new();
    let mut edited = BTreeSet::new();
    let mut placing = Vec::new();
    for (id, html) in batch {
        if let Some(old) = state.terms.remove(id) {
            before_terms.insert(*id, old);
        }
        let slot = state.slot_of(*id);
        match (html, slot) {
            (Some(html), Some(_)) => {
                state.terms.insert(*id, tokenized(*id, html));
                edited.insert(*id);
            }
            (Some(html), None) => {
                state.terms.insert(*id, tokenized(*id, html));
                placing.push(*id);
            }
            (None, Some(slot)) => state.slots[slot] = None,
            (None, None) => {}
        }
    }
    for id in placing {
        match state.slots.iter().position(Option::is_none) {
            Some(free) => state.slots[free] = Some(id),
            None => state.slots.push(Some(id)),
        }
    }
    let mut changes = Vec::new();
    for (slot, now) in state.slots.iter().enumerate() {
        let was = before_slots.get(slot).copied().flatten();
        if was != *now || now.is_some_and(|id| edited.contains(&id)) {
            changes.push((slot as u32, was, *now));
        }
    }
    (changes, before_terms)
}

/// Copy a directory tree.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A published index of 80 pages in `root/pagefind`, and an update of it
/// (three edits, two deletes, two new pages) staged in `root/staged`.
/// Returns the files a reader reaches before and after the update.
fn prepared_update(root: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut rng = Rng(0x00C0_FFEE_D00D_F00D);
    let live = root.join("pagefind");
    let mut state = State::default();
    let mut fresh = 0;
    for id in 0..80 {
        let words = 20 + rng.below(100);
        let html = page_html(&mut rng, words, &mut fresh);
        state.terms.insert(id, tokenized(id, &html));
        state.slots.push(Some(id));
    }
    let (indexes, bundle) =
        build_bundle(&BTreeMap::from([("en".to_string(), state.full_slots())])).unwrap();
    write_bundle(&bundle, &live).unwrap();
    state.plan = indexes[0].plan.clone();
    state.meta_hash = indexes[0].meta_hash.clone();
    let before = published_files(&live).unwrap();

    let mut batch = BTreeMap::new();
    for id in [3, 30, 61] {
        let html = page_html(&mut rng, 60, &mut fresh);
        batch.insert(id, Some(html));
    }
    batch.insert(10, None);
    batch.insert(44, None);
    for id in [100, 101] {
        let html = page_html(&mut rng, 60, &mut fresh);
        batch.insert(id, Some(html));
    }
    let (owned, before_terms) = apply_batch(&mut state, &batch);
    let changes: Vec<PageChange<'_>> = owned
        .iter()
        .map(|(slot, old, new)| PageChange {
            page_number: *slot,
            old: old.and_then(|id| before_terms.get(&id)).map(|t| &t.words),
            new: new.and_then(|id| state.terms.get(&id)),
        })
        .collect();
    let meta = read_meta(&live, &state.meta_hash).unwrap();
    let update = update_language(&live, "en", &meta, &state.plan, &changes).unwrap();
    let mut staged = Bundle::new();
    for (path, bytes) in &update.files {
        staged.insert(path.clone(), bytes.clone());
    }
    staged.insert(
        ENTRY_FILE.into(),
        entry_json_for(&[("en", &update.meta_hash, update.page_count)])
            .unwrap()
            .into_bytes(),
    );
    write_bundle(&staged, &root.join("staged")).unwrap();
    let mut after = update.referenced.clone();
    after.insert(ENTRY_FILE.into());
    assert!(
        update.files.len() < before.len() / 4,
        "an update of 7 pages rewrites {} of {} files",
        update.files.len(),
        before.len()
    );
    (before, after)
}

#[test]
fn a_publish_stopped_between_any_two_writes_leaves_no_reader_a_missing_file() {
    let root = tmp("stopped");
    let (before, after) = prepared_update(&root);
    let live = root.join("pagefind");

    // How many writes the publish makes.
    let dry = root.join("dry");
    copy_dir(&live, &dry);
    copy_dir(&root.join("staged"), &root.join("dry-staged"));
    let mut writes = 0;
    publish_with(&root.join("dry-staged"), &dry, "dry", &mut |_| {
        writes += 1;
        Ok(())
    })
    .unwrap();
    // A copy, so the staged files are still there for every run below.
    assert!(writes >= 3, "{writes} writes");

    for stop in 0..=writes {
        let run = root.join(format!("run{stop}"));
        copy_dir(&live, &run.join("pagefind"));
        copy_dir(&root.join("staged"), &run.join("staged"));
        let mut n = 0;
        let result = publish_with(
            &run.join("staged"),
            &run.join("pagefind"),
            "stop",
            &mut |path| {
                if n == stop {
                    anyhow::bail!("stopped before {path}");
                }
                n += 1;
                Ok(())
            },
        );
        assert_eq!(result.is_ok(), stop == writes, "stop {stop}");
        let seen = published_files(&run.join("pagefind"))
            .unwrap_or_else(|e| panic!("stopped before write {stop}: a reader meets {e:#}"));
        if stop < writes {
            assert_eq!(seen, before, "the old index until the entry flips");
        } else {
            assert_eq!(seen, after);
        }
    }
    std::fs::remove_dir_all(&root).ok();
}

/// The child half of the kill test: publish the update staged in
/// `$B218_KILL_STAGED` into `$B218_KILL_LIVE`, slowly. Does nothing unless
/// the parent test set those.
#[test]
fn kill_child() {
    let (Some(staged), Some(live)) = (
        std::env::var_os("B218_KILL_STAGED"),
        std::env::var_os("B218_KILL_LIVE"),
    ) else {
        return;
    };
    publish_with(Path::new(&staged), Path::new(&live), "kill", &mut |_| {
        std::thread::sleep(std::time::Duration::from_millis(4));
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_publish_killed_at_a_random_moment_leaves_no_reader_a_missing_file() {
    let root = tmp("killed");
    let (before, after) = prepared_update(&root);
    let mut rng = Rng(0xDEAD_BEEF_0BAD_CAFE);
    let mut outcomes = BTreeMap::from([("old", 0), ("new", 0)]);
    for i in 0..30 {
        let run = root.join(format!("run{i}"));
        copy_dir(&root.join("pagefind"), &run.join("pagefind"));
        copy_dir(&root.join("staged"), &run.join("staged"));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kill_child", "--test-threads=1", "--quiet"])
            .env("B218_KILL_STAGED", run.join("staged"))
            .env("B218_KILL_LIVE", run.join("pagefind"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(rng.below(60) as u64));
        child.kill().ok(); // SIGKILL; a no-op if it already finished
        child.wait().unwrap();
        let seen = published_files(&run.join("pagefind"))
            .unwrap_or_else(|e| panic!("killed run {i}: a reader meets {e:#}"));
        if seen == before {
            *outcomes.get_mut("old").unwrap() += 1;
        } else {
            assert_eq!(
                seen, after,
                "killed run {i}: neither the old index nor the new"
            );
            *outcomes.get_mut("new").unwrap() += 1;
        }
        // Whatever survived on disk reads back.
        read_bundle(&run.join("pagefind")).unwrap();
    }
    println!("30 killed publishes: {outcomes:?}");
    std::fs::remove_dir_all(&root).ok();
}
