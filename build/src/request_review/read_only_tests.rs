//! The review never writes to OBS. These tests read the source of the review
//! module and of the osc reads it uses, and fail on anything that could.

use std::path::Path;

use regex::Regex;

const REQUEST_READ: &str = "../osc/branch/request_read.rs";

/// The non-test source of every file of the review and of its osc reads, with
/// the file's name.
fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/request_review");
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "fetch_fake.rs" {
            files.push((name, std::fs::read_to_string(entry.path()).unwrap()));
        }
    }
    let read = dir.join(REQUEST_READ);
    files.push((
        "request_read.rs".into(),
        std::fs::read_to_string(read).unwrap(),
    ));
    for (_, text) in &mut files {
        // What follows is the file's tests, which name the words on purpose.
        *text = text.split("#[cfg(test)]").next().unwrap().to_string();
    }
    assert!(files.len() > 15, "the review's files were not found");
    files
}

#[test]
fn no_file_of_the_review_names_a_write_verb_or_an_osc_write() {
    let forbidden = [
        "\"PUT\"",
        "\"DELETE\"",
        "\"PATCH\"",
        "\"-X\"",
        "create_submit_request",
        "\"ci\"",
        "\"commit\"",
        "\"rm\"",
        "\"request\"",
        "\"branch\"",
        "\"up\"",
        "\"vc\"",
        "set_project_meta(",
        "enable_repositories(",
        "api_call(",
    ];
    for (name, text) in sources() {
        for word in forbidden {
            // The read-only adapter names the two writes it refuses.
            if name == "build_check.rs" && word.ends_with('(') {
                continue;
            }
            assert!(!text.contains(word), "{name} contains {word}");
        }
    }
}

#[test]
fn the_one_post_is_the_source_diff_and_the_one_checkout_is_a_read() {
    let mut posts = Vec::new();
    let mut checkouts = Vec::new();
    for (name, text) in sources() {
        for (what, list) in [("\"POST\"", &mut posts), ("\"co\"", &mut checkouts)] {
            for _ in text.matches(what) {
                list.push(name.clone());
            }
        }
    }
    assert_eq!(posts, ["request_read.rs"]);
    assert_eq!(checkouts, ["request_read.rs"]);
    let read = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/request_review")
            .join(REQUEST_READ),
    )
    .unwrap();
    let diff = read.split("pub fn source_diff(").nth(1).unwrap();
    let before_next = diff.split("\n}\n").next().unwrap();
    assert!(
        before_next.contains("Some(\"POST\")"),
        "the POST is not the diff's"
    );
}

/// The osc functions and types the review may name. Each is a read; a new one
/// must be added here by someone who has read what it does.
const KNOWN_OSC: &[&str] = &[
    // Modules of osc that request_read.rs uses for its own helpers.
    "error",
    "files",
    "names",
    "query",
    "DiffSpec",
    "Error",
    "ListedFile",
    "Listing",
    "OpenRequest",
    "cat_file_expanded",
    "choose_repository",
    "fetch_file_at_revision",
    "get_package_results_if_present",
    "http_status",
    "is_changes_separator",
    "open_requests_to",
    "package_entries_at_revision",
    "parse_listing",
    "parse_open_requests",
    "request_document",
    "source_diff",
    "unfinished_spec_reason",
    "BuildStatus",
    "cat_file_checked",
    "source_srcmd5",
    "self",
];

#[test]
fn the_review_names_only_known_osc_reads() {
    let re = Regex::new(r"osc::(\{[^}]*\}|\w+)").unwrap();
    let mut seen = Vec::new();
    for (name, text) in sources() {
        for cap in re.captures_iter(&text) {
            let group = &cap[1];
            let names: Vec<&str> = if let Some(inner) = group.strip_prefix('{') {
                inner
                    .trim_end_matches('}')
                    .split(',')
                    .map(str::trim)
                    .collect()
            } else {
                vec![group]
            };
            for n in names.into_iter().filter(|n| !n.is_empty()) {
                assert!(
                    KNOWN_OSC.contains(&n),
                    "{name} names osc::{n}, which no one has checked"
                );
                seen.push(n.to_string());
            }
        }
    }
    assert!(seen.iter().any(|n| n == "source_diff"));
}
