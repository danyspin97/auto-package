use super::respelling_among;

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_published_spelling_replaces_the_pypi_one() {
    let published = names(&["python313-huggingface-hub", "python314-huggingface-hub"]);
    assert_eq!(
        respelling_among("python313-huggingface_hub", &published).as_deref(),
        Some("huggingface-hub")
    );
}

#[test]
fn an_exact_match_or_another_prefix_is_no_respelling() {
    let published = names(&["python313-typing_extensions", "huggingface-hub"]);
    assert_eq!(
        respelling_among("python313-typing_extensions", &published),
        None
    );
    assert_eq!(
        respelling_among("python313-huggingface_hub", &published),
        None
    );
    assert_eq!(respelling_among("python313-unrelated", &published), None);
}
