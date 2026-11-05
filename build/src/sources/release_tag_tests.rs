//! The newest release of a forge project when its first tag is not a version.

use super::tag_list::{TagListing, version_for_tag_or_listing};

fn tags(names: &[&str]) -> Result<TagListing, String> {
    Ok(TagListing {
        tags: names.iter().map(|t| t.to_string()).collect(),
        complete: true,
    })
}

#[test]
fn a_tag_that_is_a_version_is_the_release() {
    let listed = || panic!("the listing is not asked for a version tag");
    assert_eq!(
        version_for_tag_or_listing("v1.2.0", "imaptest", listed),
        Ok(Some("1.2.0".to_string()))
    );
}

#[test]
fn a_latest_tag_gives_the_newest_version_of_the_listing() {
    let listed = || tags(&["latest", "1.9.0", "v1.10.0", "v2.0.0-rc1"]);
    assert_eq!(
        version_for_tag_or_listing("latest", "imaptest", listed),
        Ok(Some("1.10.0".to_string()))
    );
}

#[test]
fn a_tag_without_any_version_in_the_listing_is_none() {
    let listed = || tags(&["latest", "nightly"]);
    assert_eq!(
        version_for_tag_or_listing("latest", "imaptest", listed),
        Ok(None)
    );
}

#[test]
fn a_listing_that_cannot_be_read_is_an_error() {
    let listed = || Err("its tag list is unreachable".to_string());
    assert_eq!(
        version_for_tag_or_listing("latest", "imaptest", listed),
        Err("its tag list is unreachable".to_string())
    );
}
