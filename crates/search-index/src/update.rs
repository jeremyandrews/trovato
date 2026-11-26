//! The incremental updater: change some pages of one language's published
//! index and rewrite only what they touch.
//!
//! `docs/design/incremental-search-index.md` is the design. Given the
//! language's published meta file, its chunk plan (each word chunk's real
//! first and last word, which the meta holds only as prefixes) and a set of
//! page changes, [`update_language`]:
//!
//! 1. sets each changed page slot: a new or edited page gets its fragment, a
//!    freed one the language's shared tombstone fragment. A slot past the end
//!    is appended; page numbers never move;
//! 2. diffs each changed page's words against what its slot posted before
//!    (the term cache): a word whose locations are unchanged touches nothing;
//! 3. routes every word that changed to a chunk by the **frozen** ranges of
//!    the last full build: the chunk whose first and last word hold it, or for
//!    a word in a gap between two chunks the nearer neighbour (the longer
//!    common prefix, a tie to the lower chunk), whose range then widens to it;
//! 4. decodes only the touched chunks, splices their postings, and re-encodes
//!    them. A chunk past twice the cut (40,000 locations) is split by
//!    Pagefind's own rule; a chunk left empty is dropped;
//! 5. writes a new meta file whose chunk boundaries are every neighbouring
//!    pair shortened with `get_prefixes`, exactly as a full build shortens the
//!    same chunks.
//!
//! Untouched chunks and fragments keep their files and their names. Every new
//! file is content addressed and named against the live directory, so it never
//! replaces a different file under the same name. What is returned is only
//! the new files; putting them live in the safe order is
//! [`crate::publish::publish`]'s job.
//!
//! Any inconsistency between the meta, the plan, the chunks and the old words
//! (a word the cache says a page posted that its chunk does not hold, a chunk
//! that does not decode) is an error: the caller falls back to a full build,
//! which needs none of this state to be right.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::PAGEFIND_VERSION;
use crate::assemble::{
    CHUNK_LOCATIONS, ChunkPlan, bounds_meta, chunk_index, locations, tombstone_fragment,
};
use crate::codec::{
    MetaIndex, MetaPage, PackedPage, PackedWord, WordIndex, full_hash, pack, prefix_chars, to_cbor,
    unpack,
};
use crate::tokenize::PageTerms;

/// A chunk that grows past this many locations is split.
pub const SPLIT_LOCATIONS: u64 = 2 * CHUNK_LOCATIONS as u64;

/// One page slot's change.
#[derive(Debug, Clone, Copy)]
pub struct PageChange<'a> {
    /// The slot. One past the last slot appends it.
    pub page_number: u32,
    /// The words the slot's previous occupant posted, with their locations, as
    /// the term cache holds them; `None` when it was a tombstone or is new.
    pub old: Option<&'a BTreeMap<String, Vec<i32>>>,
    /// The new occupant; `None` makes the slot a tombstone.
    pub new: Option<&'a PageTerms>,
}

/// One language's index after an update.
#[derive(Debug, Clone)]
pub struct LanguageUpdate {
    /// The language key.
    pub language: String,
    /// The new meta file's stem.
    pub meta_hash: String,
    /// Page slots, tombstones included.
    pub page_count: usize,
    /// Every file this update wrote that the live directory does not already
    /// hold, by bundle path, gzipped: changed fragments, touched chunks and the
    /// meta file.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The chunk plan after the update.
    pub plan: Vec<ChunkPlan>,
    /// Every content addressed file the new meta reaches, itself included.
    pub referenced: BTreeSet<String>,
    /// The new meta, decoded.
    pub meta: MetaIndex,
    /// Chunks decoded and re-encoded.
    pub chunks_rewritten: usize,
    /// Chunks split past [`SPLIT_LOCATIONS`].
    pub chunks_split: usize,
    /// Chunks dropped because they emptied.
    pub chunks_dropped: usize,
}

/// Read and decode the meta file `pagefind.<meta_hash>.pf_meta` from `live`.
pub fn read_meta(live: &Path, meta_hash: &str) -> Result<MetaIndex> {
    let path = live.join(format!("pagefind.{meta_hash}.pf_meta"));
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    minicbor::decode(&unpack(&bytes)?).map_err(|e| anyhow!("decode {}: {e}", path.display()))
}

/// Read and decode the word chunk `index/<hash>.pf_index` from `live`.
pub fn read_chunk(live: &Path, hash: &str) -> Result<WordIndex> {
    let path = live.join(format!("index/{hash}.pf_index"));
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    minicbor::decode(&unpack(&bytes)?).map_err(|e| anyhow!("decode {}: {e}", path.display()))
}

/// Gives new files names that no different file in the live directory, nor
/// one already named in this update, holds.
struct Namer<'a> {
    live: &'a Path,
    produced: BTreeMap<String, Vec<u8>>,
}

impl Namer<'_> {
    /// The first of `stems` whose path is free or already holds exactly
    /// `bytes`, and whether the file still has to be written.
    fn name(
        &mut self,
        stems: impl Iterator<Item = String>,
        path_of: impl Fn(&str) -> String,
        bytes: &[u8],
    ) -> Result<(String, bool)> {
        let mut last = String::new();
        for stem in stems {
            let path = path_of(&stem);
            match self.produced.get(&path) {
                Some(existing) if existing == bytes => return Ok((stem, false)),
                Some(_) => {}
                None => match std::fs::read(self.live.join(&path)) {
                    Ok(existing) if existing == bytes => return Ok((stem, false)),
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        self.produced.insert(path, bytes.to_vec());
                        return Ok((stem, true));
                    }
                    Err(e) => {
                        return Err(e).with_context(|| format!("read {}", self.live.display()));
                    }
                },
            }
            last = path;
        }
        bail!("could not name {last} apart from the files already published")
    }
}

/// Pagefind's fragment names for `full` (`<lang>_<sha1>`): the short name,
/// then one character longer at a time.
fn fragment_stems(full: &str, language: &str) -> impl Iterator<Item = String> {
    let len = full.chars().count();
    let full = full.to_string();
    (language.len() + 8..=len.max(language.len() + 8)).map(move |n| prefix_chars(&full, n))
}

/// Pagefind's word chunk names for `full`: the short name, then two
/// characters longer at a time, ending at the full name.
fn chunk_stems(full: &str, language: &str) -> impl Iterator<Item = String> {
    let len = full.chars().count();
    let full = full.to_string();
    let start = language.len() + 8;
    let mut n = start;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let stem = prefix_chars(&full, n);
        if n >= len {
            done = true;
        }
        n += 2;
        Some(stem)
    })
}

/// The meta file's names: `<lang>_` and a prefix of the bare hash, from
/// `lang.len() + 8` characters, one longer at a time.
fn meta_stems(bare: &str, language: &str) -> impl Iterator<Item = String> {
    let len = bare.chars().count();
    let bare = bare.to_string();
    let language = language.to_string();
    (language.len() + 8..=len.max(language.len() + 8))
        .map(move |n| format!("{language}_{}", prefix_chars(&bare, n)))
}

/// Common prefix length, in characters.
fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Where `word` belongs by the frozen plan: the chunk whose range holds it,
/// else in a gap the nearer neighbour (the longer common prefix with that
/// neighbour's boundary word, a tie to the lower), else the first or last
/// chunk. `None` only when there are no chunks. The second value is whether
/// the word falls inside the chunk's current range.
fn route(plan: &[ChunkPlan], word: &str) -> Option<(usize, bool)> {
    if plan.is_empty() {
        return None;
    }
    let i = plan.partition_point(|c| c.last.as_str() < word);
    if i == plan.len() {
        return Some((plan.len() - 1, false));
    }
    if plan[i].first.as_str() <= word {
        return Some((i, true));
    }
    if i == 0 {
        return Some((0, false));
    }
    let lower = common_prefix(word, &plan[i - 1].last);
    let upper = common_prefix(word, &plan[i].first);
    Some(if upper > lower {
        (i, false)
    } else {
        (i - 1, false)
    })
}

/// What one touched chunk loses and gains.
#[derive(Default)]
struct Splice {
    /// Word -> page numbers whose posting goes.
    remove: BTreeMap<String, BTreeSet<u64>>,
    /// Word -> postings that arrive.
    add: BTreeMap<String, Vec<PackedPage>>,
}

/// Apply `changes` to the language index whose published meta is `meta` and
/// whose chunk plan is `plan`, reading the touched chunks from `live`. See the
/// module documentation.
pub fn update_language(
    live: &Path,
    language: &str,
    meta: &MetaIndex,
    plan: &[ChunkPlan],
    changes: &[PageChange<'_>],
) -> Result<LanguageUpdate> {
    if meta.version != PAGEFIND_VERSION {
        bail!(
            "the {language} index was written by Pagefind {}, not {PAGEFIND_VERSION}",
            meta.version
        );
    }
    if !meta.filters.is_empty() || !meta.sorts.is_empty() {
        bail!("the {language} index has filters or sorts, which the updater does not keep");
    }
    let planned: Vec<&str> = plan.iter().map(|c| c.hash.as_str()).collect();
    let published: Vec<&str> = meta.index_chunks.iter().map(|c| c.hash.as_str()).collect();
    if planned != published {
        bail!("the {language} chunk plan does not match its published meta file");
    }

    let mut namer = Namer {
        live,
        produced: BTreeMap::new(),
    };
    let mut files = BTreeMap::new();

    // 1 and 2: the slots, and what each changed page takes out and puts in.
    let mut changes: Vec<&PageChange<'_>> = changes.iter().collect();
    changes.sort_by_key(|c| c.page_number);
    let mut pages = meta.pages.clone();
    let tombstone = tombstone_fragment();
    let mut remove: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    let mut add: BTreeMap<&str, Vec<PackedPage>> = BTreeMap::new();
    let empty = BTreeMap::new();
    let mut previous: Option<u32> = None;
    for change in &changes {
        if previous == Some(change.page_number) {
            bail!("page {} changed twice in one update", change.page_number);
        }
        previous = Some(change.page_number);
        let n = change.page_number as usize;
        if n > pages.len() {
            bail!(
                "page {n} of {language} would leave a gap after {}",
                pages.len()
            );
        }
        let (fragment, word_count) = match change.new {
            Some(page) => (page.fragment.as_str(), page.word_count),
            None => (tombstone.as_str(), 0),
        };
        let full = format!("{language}_{}", full_hash(fragment.as_bytes()));
        let bytes = pack(fragment.as_bytes())?;
        let (stem, new) = namer.name(
            fragment_stems(&full, language),
            |s| format!("fragment/{s}.pf_fragment"),
            &bytes,
        )?;
        if new {
            files.insert(format!("fragment/{stem}.pf_fragment"), bytes);
        }
        let slot = MetaPage {
            hash: stem,
            word_count,
        };
        if n == pages.len() {
            pages.push(slot);
        } else {
            pages[n] = slot;
        }

        let old = change.old.unwrap_or(&empty);
        let new_words = change.new.map_or(&empty, |p| &p.words);
        for (word, locs) in old {
            if new_words.get(word) != Some(locs) {
                remove.entry(word).or_default().insert(n as u64);
            }
        }
        for (word, locs) in new_words {
            if old.get(word) != Some(locs) {
                add.entry(word).or_default().push(PackedPage {
                    page_number: n as u64,
                    locs: locs.clone(),
                });
            }
        }
    }

    // 3: route every changed word by the frozen plan.
    let mut splices: BTreeMap<usize, Splice> = BTreeMap::new();
    for (word, numbers) in remove {
        match route(plan, word) {
            Some((i, true)) => {
                splices
                    .entry(i)
                    .or_default()
                    .remove
                    .insert(word.to_string(), numbers);
            }
            _ => bail!(
                "the term cache has {word:?} in the {language} index, which holds no such word"
            ),
        }
    }
    let mut fresh: BTreeMap<String, Vec<PackedPage>> = BTreeMap::new();
    for (word, postings) in add {
        match route(plan, word) {
            Some((i, _)) => {
                splices
                    .entry(i)
                    .or_default()
                    .add
                    .insert(word.to_string(), postings);
            }
            // No chunks at all yet: the words start the first one.
            None => {
                fresh.insert(word.to_string(), postings);
            }
        }
    }

    // 4: splice the touched chunks.
    let mut next_plan: Vec<ChunkPlan> = Vec::with_capacity(plan.len() + 1);
    let mut rewritten = 0;
    let mut split = 0;
    let mut dropped = 0;
    let encode = |words: Vec<PackedWord>,
                  next_plan: &mut Vec<ChunkPlan>,
                  files: &mut BTreeMap<String, Vec<u8>>,
                  namer: &mut Namer<'_>|
     -> Result<()> {
        let bytes = to_cbor(&WordIndex {
            words: words.clone(),
        })?;
        let full = format!("{language}_{}", full_hash(&bytes));
        let packed = pack(&bytes)?;
        let (stem, new) = namer.name(
            chunk_stems(&full, language),
            |s| format!("index/{s}.pf_index"),
            &packed,
        )?;
        if new {
            files.insert(format!("index/{stem}.pf_index"), packed);
        }
        next_plan.push(ChunkPlan::of(&words, stem));
        Ok(())
    };
    let mut pieces_of = |words: Vec<PackedWord>| -> Vec<Vec<PackedWord>> {
        if words.is_empty() {
            dropped += 1;
            Vec::new()
        } else if locations(&words) > SPLIT_LOCATIONS {
            split += 1;
            chunk_index(words, CHUNK_LOCATIONS)
        } else {
            vec![words]
        }
    };

    if !fresh.is_empty() {
        let words = fresh
            .into_iter()
            .map(|(word, pages)| PackedWord { word, pages })
            .collect();
        for piece in pieces_of(words) {
            encode(piece, &mut next_plan, &mut files, &mut namer)?;
        }
    }
    for (i, chunk) in plan.iter().enumerate() {
        let Some(splice) = splices.remove(&i) else {
            next_plan.push(chunk.clone());
            continue;
        };
        rewritten += 1;
        let decoded = read_chunk(live, &chunk.hash)
            .with_context(|| format!("decode word chunk {} of {language}", chunk.hash))?;
        let mut words: BTreeMap<String, Vec<PackedPage>> = decoded
            .words
            .into_iter()
            .map(|w| (w.word, w.pages))
            .collect();
        for (word, numbers) in splice.remove {
            let postings = words.get_mut(&word).ok_or_else(|| {
                anyhow!(
                    "word chunk {} has no {word:?}, which the term cache says it holds",
                    chunk.hash
                )
            })?;
            let before = postings.len();
            postings.retain(|p| !numbers.contains(&p.page_number));
            if before - postings.len() != numbers.len() {
                bail!(
                    "word chunk {} does not post {word:?} to every page the term cache says",
                    chunk.hash
                );
            }
        }
        for (word, arriving) in splice.add {
            let postings = words.entry(word.clone()).or_default();
            for posting in arriving {
                match postings.binary_search_by_key(&posting.page_number, |p| p.page_number) {
                    Ok(_) => bail!(
                        "word chunk {} already posts {word:?} to page {}",
                        chunk.hash,
                        posting.page_number
                    ),
                    Err(at) => postings.insert(at, posting),
                }
            }
        }
        let words: Vec<PackedWord> = words
            .into_iter()
            .filter(|(_, pages)| !pages.is_empty())
            .map(|(word, pages)| PackedWord { word, pages })
            .collect();
        for piece in pieces_of(words) {
            encode(piece, &mut next_plan, &mut files, &mut namer)?;
        }
    }

    // 5: the meta file.
    let index_chunks = bounds_meta(
        next_plan
            .iter()
            .map(|c| (c.first.clone(), c.last.clone(), c.hash.clone())),
    );
    let next_meta = MetaIndex {
        version: PAGEFIND_VERSION.to_string(),
        pages,
        index_chunks,
        filters: Vec::new(),
        sorts: Vec::new(),
    };
    let meta_bytes = to_cbor(&next_meta)?;
    let packed = pack(&meta_bytes)?;
    let (meta_hash, new) = namer.name(
        meta_stems(&full_hash(&meta_bytes), language),
        |s| format!("pagefind.{s}.pf_meta"),
        &packed,
    )?;
    let meta_path = format!("pagefind.{meta_hash}.pf_meta");
    if new {
        files.insert(meta_path.clone(), packed);
    }

    let mut referenced = BTreeSet::new();
    referenced.insert(meta_path);
    for chunk in &next_meta.index_chunks {
        referenced.insert(format!("index/{}.pf_index", chunk.hash));
    }
    for page in &next_meta.pages {
        referenced.insert(format!("fragment/{}.pf_fragment", page.hash));
    }

    Ok(LanguageUpdate {
        language: language.to_string(),
        meta_hash,
        page_count: next_meta.pages.len(),
        files,
        plan: next_plan,
        referenced,
        meta: next_meta,
        chunks_rewritten: rewritten,
        chunks_split: split,
        chunks_dropped: dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(ranges: &[(&str, &str)]) -> Vec<ChunkPlan> {
        ranges
            .iter()
            .map(|(first, last)| ChunkPlan {
                first: (*first).into(),
                last: (*last).into(),
                locations: 1,
                hash: String::new(),
            })
            .collect()
    }

    #[test]
    fn a_word_goes_to_the_chunk_that_holds_it_or_the_nearer_neighbour() {
        let p = plan(&[("apple", "banana"), ("cherry", "fig"), ("grape", "kiwi")]);
        assert_eq!(route(&p, "apple"), Some((0, true)));
        assert_eq!(route(&p, "avocado"), Some((0, true)));
        assert_eq!(route(&p, "date"), Some((1, true)));
        // Before the first chunk and after the last.
        assert_eq!(route(&p, "aardvark"), Some((0, false)));
        assert_eq!(route(&p, "lemon"), Some((2, false)));
        // In the gap between banana and cherry: "bar" shares "ba" with the
        // lower boundary, "ch" shares "ch" with the upper.
        assert_eq!(route(&p, "bar"), Some((0, false)));
        assert_eq!(route(&p, "chalk"), Some((1, false)));
        // A tie (nothing shared either way) goes to the lower chunk.
        assert_eq!(route(&p, "c"), Some((1, false)));
        assert_eq!(route(&p, "fz"), Some((1, false)));
        assert_eq!(route(&[], "anything"), None);
    }

    #[test]
    fn gap_routing_keeps_the_words_in_order() {
        // For words in one gap the choice never goes back from the upper
        // neighbour to the lower, so the chunks stay sorted and disjoint.
        let p = plan(&[("ab", "abc"), ("b", "bz")]);
        let gap = ["abd", "abz", "ac", "az", "azz", "b"];
        let chosen: Vec<usize> = gap
            .iter()
            .filter_map(|w| route(&p, w))
            .map(|r| r.0)
            .collect();
        assert!(chosen.windows(2).all(|w| w[0] <= w[1]), "{chosen:?}");
    }

    #[test]
    fn stems_lengthen_by_upstream_steps() {
        let full = "en_0123456789abcdef";
        let f: Vec<String> = fragment_stems(full, "en").take(3).collect();
        assert_eq!(f, vec!["en_0123456", "en_01234567", "en_012345678"]);
        let c: Vec<String> = chunk_stems(full, "en").collect();
        assert_eq!(c.first().map(String::as_str), Some("en_0123456"));
        assert_eq!(c.get(1).map(String::as_str), Some("en_012345678"));
        assert_eq!(c.last().map(String::as_str), Some(full));
        let m: Vec<String> = meta_stems("0123456789abcdef", "en").take(2).collect();
        assert_eq!(m, vec!["en_0123456789", "en_0123456789a"]);
    }
}
