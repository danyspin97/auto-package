//! Convention 117 tells the agent what a stale `_service` source needs, and
//! that a stale one is not a reason to answer no_change.

use super::prompt::index_line;
use super::*;

#[test]
fn convention_117_moves_the_revision_and_is_never_no_change() {
    let text = crate::conventions_table::CONVENTIONS[116].1;
    assert!(text.starts_with("117. **A stale `_service` source"));
    let para = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for want in [
        "service run failed for service 'tar'",
        "Error while moving from",
        "the `revision` parameter",
        "`Version:` line",
        "`download_url` is refused",
        "Your sandbox has no sources",
        "`unverified`",
        "Answer `no_change` only when",
    ] {
        assert!(para.contains(want), "convention 117 lacks {want:?}");
    }
}

#[test]
fn the_index_routes_the_tar_service_log_to_convention_117() {
    let line = index_line(117);
    assert!(line.starts_with("117. "), "{line:?}");
    assert!(
        line.contains("service run failed for service 'tar' or Error while moving from"),
        "index line 117 lacks the trigger: {line:?}"
    );
    assert!(
        static_system_prompt().contains(line),
        "the system prompt does not carry the index line of 117"
    );
}
