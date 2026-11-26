//! Two contents can share a 7 character file name. A file from an earlier
//! publish stays on disk for the retention window, so a later index whose new
//! fragment gets the same name must not end up pointing at the old bytes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use trovato_search_index::codec::{full_hash, short_name};
use trovato_search_index::decode::{decode_bundle, read_bundle};
use trovato_search_index::publish::{publish, write_bundle};
use trovato_search_index::{PageTerms, Slot, build_bundle, build_bundle_for_live};

fn fragment(i: u32) -> String {
    format!(
        "{{\"url\":\"/p{i}.html\",\"content\":\"page {i}\",\"word_count\":1,\
         \"filters\":{{}},\"meta\":{{}},\"anchors\":[]}}"
    )
}

fn page(i: u32) -> PageTerms {
    PageTerms {
        language: "en".into(),
        word_count: 1,
        fragment: fragment(i),
        words: BTreeMap::from([("page".to_string(), vec![0])]),
    }
}

/// Two page numbers whose fragments get the same short file name.
fn clashing_pair() -> (u32, u32) {
    let mut seen: HashMap<String, u32> = HashMap::new();
    for i in 0..10_000_000 {
        let full = format!("en_{}", full_hash(fragment(i).as_bytes()));
        if let Some(j) = seen.insert(short_name(&full, "en"), i) {
            return (j, i);
        }
    }
    panic!("no clash found");
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "b217-clash-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build one page's index against `live` and publish it there.
fn publish_one(root: &Path, live: &Path, terms: &PageTerms, round: &str) {
    let slots = BTreeMap::from([("en".to_string(), vec![Slot::Page(terms)])]);
    let (_, bundle) = build_bundle_for_live(&slots, live).unwrap();
    let staged = root.join(format!(".pagefind_build_{round}"));
    write_bundle(&bundle, &staged).unwrap();
    publish(&staged, live, round).unwrap();
}

/// The URL of every page the live entry reaches.
fn live_urls(live: &Path) -> Vec<String> {
    let decoded = decode_bundle(&read_bundle(live).unwrap()).unwrap();
    decoded
        .iter()
        .flat_map(|index| index.pages().unwrap().into_keys())
        .collect()
}

#[test]
fn a_new_file_never_takes_the_name_of_a_different_retained_one() {
    let (a, b) = clashing_pair();
    let root = tmp("live");
    let live = root.join("pagefind");

    let first = page(a);
    publish_one(&root, &live, &first, "one");
    assert_eq!(live_urls(&live), vec![format!("/p{a}.html")]);

    // The next index has only the other page, whose fragment's short name is
    // the retained file's.
    let second = page(b);
    publish_one(&root, &live, &second, "two");
    assert_eq!(
        live_urls(&live),
        vec![format!("/p{b}.html")],
        "the new meta reaches the new fragment, not the retained one"
    );

    // The retained file is untouched, for readers of the first meta.
    let short = short_name(
        &format!("en_{}", full_hash(first.fragment.as_bytes())),
        "en",
    );
    let retained = std::fs::read(live.join(format!("fragment/{short}.pf_fragment"))).unwrap();
    let unpacked = trovato_search_index::codec::unpack(&retained).unwrap();
    assert_eq!(String::from_utf8(unpacked).unwrap(), first.fragment);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn publishing_a_bundle_that_reuses_a_different_files_name_is_refused() {
    let (a, b) = clashing_pair();
    let root = tmp("refuse");
    let live = root.join("pagefind");
    let first = page(a);
    publish_one(&root, &live, &first, "one");
    let entry_before = std::fs::read(live.join("pagefind-entry.json")).unwrap();

    // Built without looking at the live directory: Pagefind's own names.
    let second = page(b);
    let slots = BTreeMap::from([("en".to_string(), vec![Slot::Page(&second)])]);
    let (_, bundle) = build_bundle(&slots).unwrap();
    let staged = root.join(".pagefind_build_two");
    write_bundle(&bundle, &staged).unwrap();
    let err = publish(&staged, &live, "two").unwrap_err();
    assert!(
        format!("{err:#}").contains("reuses the name of a different file"),
        "{err:#}"
    );
    assert_eq!(
        std::fs::read(live.join("pagefind-entry.json")).unwrap(),
        entry_before,
        "the published entry is left as it was"
    );
    std::fs::remove_dir_all(&root).ok();
}
