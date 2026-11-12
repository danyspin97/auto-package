//! The wording of a `FilesNotOwned` failure on one line: the first few
//! unowned directories, then how many more there are.

/// How many paths a line names before it says `+N more`.
const SHOWN: usize = 3;

/// `files not owned: /a, /b (+1 more)`, naming at most [`SHOWN`] paths in the
/// order the log gave them. A list with no paths reads as the bare kind.
/// Paths are never cut short, so a long one is shown whole.
pub fn files_not_owned(paths: &[String]) -> String {
    if paths.is_empty() {
        return "files not owned".to_string();
    }
    let shown: Vec<&str> = paths.iter().take(SHOWN).map(String::as_str).collect();
    let more = paths.len() - shown.len();
    if more == 0 {
        format!("files not owned: {}", shown.join(", "))
    } else {
        format!("files not owned: {} (+{more} more)", shown.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::files_not_owned;

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_path_is_named_with_no_count() {
        assert_eq!(
            files_not_owned(&paths(&["/usr/share/wayland-sessions"])),
            "files not owned: /usr/share/wayland-sessions"
        );
    }

    #[test]
    fn three_paths_are_all_named_in_order() {
        assert_eq!(
            files_not_owned(&paths(&[
                "/usr/share/wayland-sessions",
                "/usr/share/xdg-desktop-portal",
                "/usr/share/xdg-desktop-portal/portals",
            ])),
            "files not owned: /usr/share/wayland-sessions, /usr/share/xdg-desktop-portal, /usr/share/xdg-desktop-portal/portals"
        );
    }

    #[test]
    fn a_fourth_path_is_counted_not_named() {
        assert_eq!(
            files_not_owned(&paths(&["/a", "/b", "/c", "/d"])),
            "files not owned: /a, /b, /c (+1 more)"
        );
    }

    #[test]
    fn five_paths_name_three_and_count_two() {
        assert_eq!(
            files_not_owned(&paths(&["/a", "/b", "/c", "/d", "/e"])),
            "files not owned: /a, /b, /c (+2 more)"
        );
    }

    #[test]
    fn a_long_path_is_shown_whole() {
        let long = format!("/usr/share/{}", "x".repeat(300));
        let out = files_not_owned(std::slice::from_ref(&long));
        assert_eq!(out, format!("files not owned: {long}"));
    }

    #[test]
    fn cjk_paths_are_kept_whole_and_counted_in_paths_not_bytes() {
        let names = paths(&[
            "/usr/share/日本語",
            "/usr/share/漢字の辞書",
            "/usr/share/データ",
            "/x",
        ]);
        assert_eq!(
            files_not_owned(&names),
            "files not owned: /usr/share/日本語, /usr/share/漢字の辞書, /usr/share/データ (+1 more)"
        );
    }

    #[test]
    fn no_paths_reads_as_the_bare_kind() {
        assert_eq!(files_not_owned(&[]), "files not owned");
    }
}
