use super::*;

/// python-celery: OBS names `kombu >= 5.7.0` for both flavours and says
/// nothing about the version the repository has.
#[test]
fn a_bound_without_a_got_version_is_read() {
    let detail = vec![
        "nothing provides python313-kombu >= 5.7.0".to_string(),
        "nothing provides python314-kombu >= 5.7.0".to_string(),
    ];
    assert_eq!(
        bounded_missing(&detail),
        [("kombu".to_string(), "5.7.0".to_string())]
    );
    // Not an outdated package: nothing says what the repository has.
    assert!(outdated_packages(&detail).is_empty());
}

/// A line that says what the repository has is an outdated package, and
/// `outdated_packages` already reads it: it is left to that reader.
#[test]
fn a_bound_with_a_got_version_is_left_to_the_outdated_reader() {
    let detail = vec![
        "nothing provides python313-kombu >= 5.7.0".to_string(),
        "(got version 5.6.2-1.1)".to_string(),
        "nothing provides python314-kombu >= 5.7.0, (got version 5.6.2-1.1)".to_string(),
    ];
    assert!(bounded_missing(&detail).is_empty());
}

/// Lines without a version bound, and rich dependencies, are not bounds.
#[test]
fn a_name_without_a_bound_is_not_read() {
    let detail = vec![
        "nothing provides python313-logfire".to_string(),
        "nothing provides (python314-trio >= 0.25.1 with python314-trio < 0.34)".to_string(),
        "nothing provides python313-attrs >= not-a-version".to_string(),
    ];
    assert!(bounded_missing(&detail).is_empty());
}

/// Two flavours of one package give one entry, the first bound read.
#[test]
fn flavours_of_one_package_give_one_entry() {
    let detail = vec![
        "nothing provides python313-billiard >= 4.3.2".to_string(),
        "nothing provides python314-billiard >= 4.3.2".to_string(),
    ];
    let out = bounded_missing(&detail);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1, "4.3.2");
}
