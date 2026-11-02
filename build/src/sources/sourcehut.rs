//! SourceHut (`git.sr.ht`) repositories. Their owners are `~user`.

/// The SourceHut git host, matched case-insensitively.
pub(crate) fn is_sourcehut_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("git.sr.ht")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sourcehut_host_is_sourcehut() {
        assert!(is_sourcehut_host("git.sr.ht"));
        assert!(is_sourcehut_host("GIT.sr.ht"));
        assert!(!is_sourcehut_host("sr.ht.example.org"));
        assert!(!is_sourcehut_host("github.com"));
    }
}

#[cfg(test)]
mod url_tests {
    use crate::sources::owner_repo;

    #[test]
    fn a_sourcehut_owner_keeps_its_tilde_and_a_download_path_is_the_repo() {
        let (host, project_path, repo) = owner_repo(
            "https://git.sr.ht/~emersion/kanshi/refs/download/v1.6.0/kanshi-1.6.0.tar.gz",
        )
        .unwrap();
        assert_eq!(host, "git.sr.ht");
        assert_eq!(project_path, "~emersion/kanshi");
        assert_eq!(repo, "kanshi");
        assert!(owner_repo("https://github.com/~emersion/kanshi").is_err());
        assert!(owner_repo("https://git.sr.ht/~/kanshi").is_err());
    }
}
