use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn url(u: &str) -> Option<(OriginKind, String, Option<u64>)> {
    classify_url(u, OriginEvidence::PatchUrl).map(|o| (o.kind, o.url, o.number))
}

#[test]
fn github_and_gitea_request_and_commit_shapes_are_origins() {
    assert_eq!(
        url("https://github.com/fwupd/fwupd-efi/pull/106"),
        Some((
            OriginKind::Pr,
            "https://github.com/fwupd/fwupd-efi/pull/106".into(),
            Some(106)
        ))
    );
    for tail in [".patch", ".diff", "/files", "/commits"] {
        let u = format!("https://github.com/o/r/pull/7{tail}");
        assert_eq!(url(&u).map(|t| t.0), Some(OriginKind::Pr), "{u}");
    }
    let commit = format!("https://github.com/o/r/pull/7/commits/{SHA}");
    let got = classify_url(&commit, OriginEvidence::PatchUrl).unwrap();
    assert_eq!(got.kind, OriginKind::Commit);
    assert_eq!(got.url, format!("https://github.com/o/r/commit/{SHA}"));
    assert_eq!(got.commit.as_deref(), Some(SHA));
    let commit = format!("https://github.com/o/r/commit/{SHA}.patch");
    assert_eq!(url(&commit).map(|t| t.0), Some(OriginKind::Commit));
    let pulls = "https://codeberg.org/o/r/pulls/3";
    assert_eq!(
        url(pulls),
        Some((
            OriginKind::Pr,
            "https://codeberg.org/o/r/pulls/3".into(),
            Some(3)
        ))
    );
    let gitea = format!("https://gitea.example.org/o/r/commit/{SHA}");
    assert_eq!(url(&gitea).map(|t| t.0), Some(OriginKind::Commit));
}

#[test]
fn gitlab_merge_requests_and_commits_are_origins() {
    let mr = classify_url(
        "https://gitlab.com/g/sub/p/-/merge_requests/7.patch",
        OriginEvidence::PatchUrl,
    )
    .unwrap();
    assert_eq!(mr.kind, OriginKind::Mr);
    assert_eq!(mr.project, "https://gitlab.com/g/sub/p");
    assert_eq!(mr.url, "https://gitlab.com/g/sub/p/-/merge_requests/7");
    let commit = format!("https://gitlab.gnome.org/g/p/-/commit/{SHA}");
    assert_eq!(url(&commit).map(|t| t.0), Some(OriginKind::Commit));
}

#[test]
fn other_urls_are_not_origins() {
    for u in [
        "https://github.com/o/r/issues/106",
        "https://github.com/o/r/archive/v1.2.tar.gz",
        "https://bugzilla.suse.com/attachment.cgi?id=1",
        "https://src.fedoraproject.org/rpms/x/raw/main/f/106.patch",
        "https://salsa.debian.org/go-team/x/-/merge_requests/3",
        "https://gitlab.archlinux.org/archlinux/packaging/packages/x/-/merge_requests/3",
        "https://build.opensuse.org/package/view_file/x/106.patch",
        "http://github.com/o/r/pull/1",
        "https://github.com/o/../pull/1",
        "https://lists.sr.ht/~u/list/patches/1",
        "https://github.com/o/r/pull/x",
    ] {
        assert!(url(u).is_none(), "{u}");
    }
}

fn read(pkg: &str, spec: &str, changes: &str, name: &str, text: &str) -> OriginRead {
    read_origin(&OriginInputs {
        pkg,
        spec,
        changes,
        name,
        text,
    })
}

const SPEC: &str =
    "Name: fwupd-efi\nVersion: 1.4\nURL: https://github.com/fwupd/fwupd-efi\nPatch0: 106.patch\n";

#[test]
fn a_header_upstream_line_is_the_origin() {
    let text = "Subject: [PATCH] x\nUpstream: submitted https://github.com/o/r/pull/106\n\ndiff";
    let got = read("p", SPEC, "", "106.patch", text);
    let OriginRead::Found(o) = got else {
        panic!("{got:?}")
    };
    assert_eq!(o.url, "https://github.com/o/r/pull/106");
    assert_eq!(o.evidence, OriginEvidence::PatchHeader);
}

#[test]
fn a_gh_reference_in_references_is_a_pull_request() {
    let text = "References: gh#o/r#12\nSubject: x\n\n---\n";
    let OriginRead::Found(o) = read("p", SPEC, "", "1.patch", text) else {
        panic!()
    };
    assert_eq!(o.url, "https://github.com/o/r/pull/12");
}

#[test]
fn git_repo_and_commit_make_a_commit_origin() {
    let text = format!("Git-repo: https://github.com/o/r.git\nGit-commit: {SHA}\n\n---\n");
    let OriginRead::Found(o) = read("p", SPEC, "", "1.patch", &text) else {
        panic!()
    };
    assert_eq!(o.kind, OriginKind::Commit);
    assert_eq!(o.url, format!("https://github.com/o/r/commit/{SHA}"));
}

#[test]
fn a_spec_comment_above_the_patch_names_the_origin() {
    let spec = "Name: x\nURL: https://example.org/x\n# PATCH-FIX-UPSTREAM gh#o/r#9 -- fix\nPatch0: fix.patch\n";
    let OriginRead::Found(o) = read("x", spec, "", "fix.patch", "Subject: x\n") else {
        panic!()
    };
    assert_eq!(o.url, "https://github.com/o/r/pull/9");
    assert_eq!(o.evidence, OriginEvidence::SpecComment);
}

#[test]
fn a_comment_separated_by_a_blank_line_is_not_read() {
    let spec = "Name: x\n# https://github.com/o/r/pull/9\n\nPatch0: fix.patch\n";
    assert_eq!(
        read("x", spec, "", "fix.patch", "Subject: x\n"),
        OriginRead::None
    );
}

#[test]
fn a_patch_url_is_the_origin() {
    let spec = "Name: x\nPatch0: https://github.com/o/r/pull/106.patch#/106.patch\n";
    let OriginRead::Found(o) = read("x", spec, "", "106.patch", "Subject: x\n") else {
        panic!()
    };
    assert_eq!(o.evidence, OriginEvidence::PatchUrl);
    assert_eq!(o.url, "https://github.com/o/r/pull/106");
}

#[test]
fn a_changes_bullet_that_names_the_file_gives_its_url() {
    let changes = "-------------------------------------------------------------------\n\
Mon Jul 1 2025 - someone\n\n- Add 106.patch: fixes the thing\n  (https://github.com/o/r/pull/106)\n";
    let OriginRead::Found(o) = read(
        "x",
        "Name: x\nPatch0: 106.patch\n",
        changes,
        "106.patch",
        "Subject: x\n",
    ) else {
        panic!()
    };
    assert_eq!(o.evidence, OriginEvidence::ChangesBullet);
    assert_eq!(o.number, Some(106));
}

#[test]
fn a_file_name_alone_is_an_origin_on_github() {
    let OriginRead::Found(o) = read("fwupd-efi", SPEC, "", "106.patch", "Subject: x\n") else {
        panic!()
    };
    assert_eq!(o.evidence, OriginEvidence::FileName);
    assert_eq!(o.url, "https://github.com/fwupd/fwupd-efi/pull/106");
}

#[test]
fn file_name_patterns_refuse_numbering_of_the_package() {
    for name in [
        "0106.patch",
        "pkg-3.patch",
        "fix-106.patch",
        "1234567.patch",
        "other-pr-3.patch",
    ] {
        assert_eq!(
            read("pkg", SPEC, "", name, "Subject: x\n"),
            OriginRead::None,
            "{name}"
        );
    }
    for name in [
        "pr-106.patch",
        "pull_106.patch",
        "fwupd-efi-gh-106.patch",
        "gh106.patch",
    ] {
        assert!(
            matches!(
                read("fwupd-efi", SPEC, "", name, "Subject: x\n"),
                OriginRead::Found(_)
            ),
            "{name}"
        );
    }
}

#[test]
fn mr_is_only_read_on_a_gitlab_project() {
    let gitlab = "Name: x\nVersion: 1\nURL: https://gitlab.com/g/p\nPatch0: mr-7.patch\n";
    let OriginRead::Found(o) = read("x", gitlab, "", "mr-7.patch", "Subject: x\n") else {
        panic!()
    };
    assert_eq!(o.kind, OriginKind::Mr);
    assert_eq!(
        read("x", SPEC, "", "mr-7.patch", "Subject: x\n"),
        OriginRead::None
    );
}

#[test]
fn two_different_requests_are_a_conflict() {
    let text = "Upstream: https://github.com/o/r/pull/106\n\n---\n";
    let changes = "- Add 106.patch: from https://github.com/o/r/pull/107\n";
    let got = read("x", SPEC, changes, "106.patch", text);
    assert_eq!(
        got,
        OriginRead::Conflict(vec![
            "https://github.com/o/r/pull/106".into(),
            "https://github.com/o/r/pull/107".into(),
        ])
    );
}

#[test]
fn an_issue_url_beside_the_pull_request_is_skipped() {
    let text = "Upstream: https://github.com/o/r/issues/5 https://github.com/o/r/pull/106\n\n---\n";
    let OriginRead::Found(o) = read("x", SPEC, "", "106.patch", text) else {
        panic!()
    };
    assert_eq!(o.url, "https://github.com/o/r/pull/106");
}

#[test]
fn a_file_name_does_not_overrule_a_header() {
    let text = "Upstream: https://github.com/o/r/pull/200\n\n---\n";
    let OriginRead::Found(o) = read("fwupd-efi", SPEC, "", "106.patch", text) else {
        panic!()
    };
    assert_eq!(o.url, "https://github.com/o/r/pull/200");
}
