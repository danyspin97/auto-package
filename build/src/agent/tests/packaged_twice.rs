//! Convention 118 tells the agent how a file that two flavour subpackages own
//! is shipped, and the index routes the `50-check-packaged-twice` log to it.

use super::prompt::index_line;
use super::*;

#[test]
fn convention_118_ships_a_shared_file_from_one_flavour() {
    let text = crate::conventions_table::CONVENTIONS[117].1;
    assert!(text.starts_with("118. **A file that two flavour subpackages both own"));
    let para = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for want in [
        "`50-check-packaged-twice`",
        "found conflict of A with B:",
        "%{python_flavor}-X-devel",
        "`%exclude`",
        "`%python_expand`",
        "Do not drop a flavour's `-devel` package",
        "Do not answer `no_change` by saying the build succeeded",
    ] {
        assert!(para.contains(want), "convention 118 lacks {want:?}");
    }
}

#[test]
fn the_index_routes_the_packaged_twice_block_to_convention_118() {
    let line = index_line(118);
    assert!(line.starts_with("118. "), "{line:?}");
    assert!(
        line.contains("found conflict of A with B:") && line.contains("50-check-packaged-twice"),
        "index line 118 lacks the trigger: {line:?}"
    );
    assert!(
        static_system_prompt().contains(line),
        "the system prompt does not carry the index line of 118"
    );
}
