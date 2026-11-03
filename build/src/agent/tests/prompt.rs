//! Prompt and conventions tests: what the turn is told, and the review prompts.

use super::*;
use crate::agent::build_prompt::*;

/// The conventions are cross-referenced by number in the prompt itself
/// ("see convention 9"), so a duplicate or skipped number silently points
/// the model at the wrong rule.
#[test]
fn settings_env_block_is_read_and_osc_dropped() {
    let env = parse_settings_env(
        r#"{"model":"x","env":{"CLAUDE_CODE_USE_VERTEX":"1","CLOUD_ML_REGION":"global",
               "N":3,"OSC_PASS":"p","NESTED":{"a":1}}}"#,
    );
    assert_eq!(
        env.get("CLAUDE_CODE_USE_VERTEX").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        env.get("CLOUD_ML_REGION").map(String::as_str),
        Some("global")
    );
    assert_eq!(env.get("N").map(String::as_str), Some("3"));
    assert!(!env.contains_key("OSC_PASS") && !env.contains_key("NESTED"));
    assert!(parse_settings_env("not json").is_empty());
    assert!(parse_settings_env(r#"{"model":"x"}"#).is_empty());
}

/// Every convention's full text, as one string.
pub(super) fn all_conventions() -> String {
    crate::conventions_table::CONVENTIONS
        .iter()
        .map(|(_, t)| *t)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn conventions_are_numbered_consecutively_from_one() {
    for (i, (n, text)) in crate::conventions_table::CONVENTIONS.iter().enumerate() {
        assert_eq!(*n as usize, i + 1, "convention numbering is not 1..n");
        assert!(
            text.starts_with(&format!("{n}. ")),
            "file {n}.md opens with another number"
        );
    }
}

#[test]
fn the_index_has_one_line_per_convention() {
    let lines: Vec<&str> = CONVENTIONS_INDEX.lines().collect();
    assert_eq!(lines.len(), crate::conventions_table::CONVENTIONS.len());
    for (i, l) in lines.iter().enumerate() {
        assert!(
            l.starts_with(&format!("{}. ", i + 1)),
            "index line {} is {l:?}",
            i + 1
        );
        assert!(l.len() <= 200, "index line {} is too long", i + 1);
    }
    for n in INLINE_CONVENTIONS {
        assert!(
            lines[n as usize - 1].contains("[inline]"),
            "convention {n} is not marked inline"
        );
    }
}

#[test]
fn the_system_prompt_holds_the_inline_conventions_and_the_index_only() {
    let p = static_system_prompt();
    assert!(p.contains("27. **ANY dependency missing from OBS"));
    assert!(!p.contains("do not guess a repository URL"));
    assert!(!p.contains("Do not guess a repository URL"));
    assert!(p.contains("Convention index"));
    assert!(p.len() < 45_000, "system prompt is {} bytes", p.len());
}

#[test]
fn materialising_writes_every_convention_and_is_idempotent() {
    let ws = tempfile::tempdir().unwrap();
    materialise_conventions(ws.path()).unwrap();
    materialise_conventions(ws.path()).unwrap();
    let n = std::fs::read_dir(ws.path().join(CONVENTIONS_DIR))
        .unwrap()
        .count();
    assert_eq!(n, crate::conventions_table::CONVENTIONS.len());
    let got = std::fs::read_to_string(ws.path().join("conventions/76.md")).unwrap();
    assert!(got.starts_with("76. "));
}

#[test]
fn the_turn_may_not_rewrite_the_convention_files() {
    let ws = std::path::Path::new("/tmp/ws");
    let denied = turn_denied_tools(ws, None);
    assert!(
        denied.contains("Write(//tmp/ws/conventions/**)") || denied.contains("/conventions/**")
    );
}

pub(super) fn request<'a>(
    ws: &'a std::path::Path,
    subprojects: &'a [(String, String)],
) -> TurnRequest<'a> {
    TurnRequest {
        workspace: ws,
        pkg_name: "cuda-python",
        version: "13.0.2",
        is_python: true,
        task: Task::Review,
        attempt: 0,
        max_attempts: 10,
        spent_usd: 0.0,
        budget_usd: 2.0,
        hint: None,
        notes: "",
        package_notes: "",
        from_repo_archive: true,
        subprojects,
        unresolvable_deps: &[],
        unresolvable_detail: &[],
        failed_deps: &[],
        unbuildable_deps: &[],
        session_id: None,
        checkout_dir: None,
        context: None,
        carried: false,
        rpms_dir: None,
        sibling_dirs: &[],
        model_rotation: 0,
        on_tool_use: None,
    }
}

#[test]
fn models_escalate_with_the_attempt_and_rotate_on_demand() {
    assert_eq!(model_for_attempt(1, 0), "claude-haiku-5-5");
    assert_eq!(model_for_attempt(3, 0), "claude-sonnet-5-5");
    assert_eq!(model_for_attempt(6, 0), "claude-opus-5-5");
    // after a 429 the same turn moves to the next model, wrapping around
    assert_eq!(model_for_attempt(1, 1), "claude-sonnet-5-5");
    assert_eq!(model_for_attempt(1, 2), "claude-opus-5-5");
    assert_eq!(model_for_attempt(6, 1), "claude-haiku-5-5");
    // every rotation of one attempt yields a distinct model until it wraps
    let seen: std::collections::HashSet<_> = (0..3).map(|r| model_for_attempt(4, r)).collect();
    assert_eq!(seen.len(), 3);
}

#[test]
fn a_monorepos_sibling_projects_reach_the_prompt() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let subs = vec![
        ("cuda_bindings".to_string(), "cuda-bindings".to_string()),
        ("cuda_python".to_string(), "cuda-python".to_string()),
    ];
    let prompt = build_turn_prompt(&request(ws, &subs));

    assert!(
        prompt.contains("## Other projects in this archive"),
        "{prompt}"
    );
    assert!(prompt.contains("cuda_bindings/  (declares itself `cuda-bindings`)"));
    assert!(prompt.contains("convention 30"));
    // And the repository-archive note, since this did not come from a sdist.
    assert!(prompt.contains("archive of the upstream repository"));
}

/// The source note describes the archive the turn has: a Python project from
/// a repository, a C/C++ project from a forge archive, or nothing for a sdist.
#[test]
fn the_source_note_describes_the_archive_it_is_given() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);

    let python = build_turn_prompt(&req);
    assert!(python.contains("not a PyPI sdist"), "{python}");
    assert!(python.contains("Run `ls sdist/` FIRST"));

    req.is_python = false;
    let forge = build_turn_prompt(&req);
    assert!(forge.contains("(a forge tag), not a release tarball"));
    assert!(forge.contains("`source/` is the one directory inside it"));
    assert!(!forge.contains("PyPI"), "{forge}");

    req.from_repo_archive = false;
    let sdist = build_turn_prompt(&req);
    assert!(!sdist.contains("archive of the upstream repository"));
    assert!(!sdist.contains("Source taken from a repository"));
}

/// The mechanically-recognised cause leads the hint section, before the
/// excerpt, so the agent acts on it instead of re-reading the log.
#[test]
fn a_probable_cause_leads_the_hint_section() {
    let log_text = [
        "[   1s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
        "[   2s] Traceback (most recent call last):",
        "[   3s] ModuleNotFoundError: No module named 'transformers'",
        "[   4s] error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)",
    ]
    .join("\n");
    let hint = crate::log::extract(
        &log_text,
        &crate::log::PathMapper::with_sdist_root("cuda-python-1.0"),
    );
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.hint = Some(&hint);
    let prompt = build_turn_prompt(&req);

    let cause = prompt
        .find("## Probable cause (read from the build log by auto-package)")
        .expect("the cause section is present");
    let excerpt = prompt
        .find("heuristic extract of the build log")
        .expect("the heuristic hint is still there");
    assert!(cause < excerpt, "{prompt}");
    assert!(prompt.contains("%{python_module transformers}"), "{prompt}");
}

#[test]
fn a_single_project_archive_says_nothing_about_subpackages() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let prompt = build_turn_prompt(&request(ws, &[]));
    assert!(!prompt.contains("## Other projects in this archive"));
}

/// Regression test for the whole point of `static_system_prompt`: the
/// per-turn user prompt must NOT contain the static conventions/workflow
/// text, or re-inlining it here would silently defeat prompt caching
/// (Anthropic's cache only hits on a byte-identical prefix, and this
/// prompt differs on every call by construction — package name, spec
/// content, notes.md, ...). The static content belongs only in
/// `--append-system-prompt`, sent once per turn as its own argument.
#[test]
fn static_conventions_and_workflow_text_are_not_inlined_into_the_user_prompt() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let prompt = build_turn_prompt(&request(ws, &[]));
    assert!(
        !prompt.contains("29. Missing source code"),
        "a CONVENTIONS entry leaked into the per-turn user prompt"
    );
    assert!(
        !prompt.contains("### Finding Missing C Headers and Libraries"),
        "STATIC_WORKFLOW leaked into the per-turn user prompt"
    );
}

/// The prompt is handed to `claude` by file because it no longer fits
/// comfortably in one argv entry (131072-byte cap); the file must hold
/// exactly the cached text.
#[test]
fn the_redist_cuda_prompt_names_the_generated_packages_only_with_the_mode_on() {
    {
        let _off = crate::redist::TestMode::set(false);
        assert!(redist_cuda_prompt().is_none());
    }
    let _on = crate::redist::TestMode::set(true);
    let p = redist_cuda_prompt().unwrap();
    for pkg in [
        "`cuda-cudart-devel`",
        "`cuda-cupti-devel`",
        "`cuda-nvrtc-devel`",
        "`cuda-cusparse-lt-devel`",
        "`cuda-nvshmem-devel`",
    ] {
        assert!(p.contains(pkg), "{pkg}");
    }
    assert!(p.contains("CUDA_HOME=/usr/local/cuda-<major>.<minor>"));
    assert!(p.contains("nvcc"));
    // The wheel-mode text is untouched.
    assert!(!static_system_prompt().is_empty());
}

#[test]
fn system_prompt_file_holds_the_static_prompt() {
    use std::os::unix::fs::PermissionsExt;
    let file = SystemPromptFile::create().unwrap();
    assert_eq!(
        std::fs::read_to_string(&file.path).unwrap(),
        static_system_prompt()
    );
    let dir = file.path.parent().unwrap();
    let mode = std::fs::metadata(dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    let kept = dir.to_path_buf();
    drop(file);
    assert!(
        !kept.exists(),
        "the prompt copy must be removed with the turn"
    );
}

/// The Bash rules reach every turn, and name what the sandbox refuses.
#[test]
fn static_system_prompt_states_the_bash_grants() {
    assert!(static_system_prompt().contains(BASH_RULES.trim()));
    assert!(
        BASH_RULES.len() < 600,
        "Bash rules are {} bytes",
        BASH_RULES.len()
    );
    for want in [
        "`ls <relative path>`",
        "`quilt <subcommand>`",
        "`;`",
        "globs",
    ] {
        assert!(BASH_RULES.contains(want), "Bash rules lack {want}");
    }
}

#[test]
fn static_system_prompt_carries_both_conventions_and_workflow() {
    let combined = static_system_prompt();
    assert!(combined.contains("29. Merged into convention 77"));
    assert!(combined.contains("### Finding Missing C Headers and Libraries"));
    assert!(combined.contains("## Workflow"));
}

/// python-nvidia-cutlass-dsl-libs-cu13 failed `%check` with `No module named
/// '...cutlass._mlir'`: the noarch `-core` (the `__init__.py`) is in sitelib,
/// its arch-specific siblings were in sitearch. Convention 76 names the
/// symptom and says which tree wins and which siblings move.
#[test]
fn convention_76_names_the_split_tree_symptom_and_the_fix() {
    let text = crate::conventions_table::CONVENTIONS[75].1;
    assert!(text.starts_with("76. **The files of one Python package come from one tree.**"));
    for want in [
        "ModuleNotFoundError: No module",
        "`py3-none-any` wheel",
        "moves its tree",
        "fix all siblings",
    ] {
        assert!(text.contains(want), "convention 76 lacks {want:?}");
    }
}

/// python-cffi 16.0/16.1 failed `%check` with `command 'g++' failed` on a
/// C-only package, and the spec had no compiler BuildRequires. The index must
/// route that symptom to convention 10, and convention 10 must say cffi needs
/// both gcc and gcc-c++.
#[test]
fn a_missing_compiler_routes_to_convention_10_and_cffi_needs_gcc_cxx() {
    let line = CONVENTIONS_INDEX.lines().nth(9).unwrap();
    assert!(line.starts_with("10. "), "index line 10 is {line:?}");
    assert!(
        line.contains("compiler") && line.contains("'g++'") && line.contains("'gcc'"),
        "index line 10 lacks the compiler trigger: {line:?}"
    );
    let text = crate::conventions_table::CONVENTIONS[9].1;
    assert!(text.starts_with("10. **Test failures"));
    let at = text
        .find("**Missing compiler**")
        .expect("convention 10 has a missing-compiler paragraph");
    let rest = &text[at..];
    let para = &rest[..rest.find("For any other").unwrap_or(rest.len())];
    for want in [
        "cffi",
        "`BuildRequires: gcc`",
        "`BuildRequires: gcc-c++`",
        "named in the error",
    ] {
        assert!(
            para.contains(want),
            "missing-compiler paragraph lacks {want:?}"
        );
    }
}

/// The priorities frame every convention, so they must come first, and
/// must name both halves of the goal: time, and building AND working.
#[test]
fn the_post_build_review_prompt_is_a_cautious_cleanup() {
    let ws = std::path::Path::new("/tmp/ws");
    let mut req = request(ws, &[]);
    req.task = Task::PostBuildReview;
    req.attempt = 1;
    let prompt = build_turn_prompt(&req);
    assert!(prompt.contains("just BUILT"));
    assert!(prompt.contains("no_change"));
    assert!(prompt.contains("Post-build review."));
}

#[test]
fn the_patch_review_prompt_names_the_verdicts_and_its_budget() {
    let ws = std::path::Path::new("/tmp/ws");
    let mut req = request(ws, &[]);
    req.task = Task::PatchReview;
    let prompt = build_turn_prompt(&req);
    for want in [
        "ONE patch",
        "`accept`",
        "`rewrite`",
        "`reject`",
        "Patch review. $2.00",
    ] {
        assert!(prompt.contains(want), "missing {want:?}");
    }
}

#[test]
fn static_system_prompt_opens_with_the_priorities() {
    let combined = static_system_prompt();
    assert!(
        combined
            .trim_start()
            .starts_with("# What matters in this turn")
    );
    assert!(combined.contains("**Time matters.**"));
    assert!(combined.contains("builds AND works"));
    assert!(
        combined.find("**Time matters.**") < combined.find("openSUSE spec conventions"),
        "the priorities must precede the conventions"
    );
}

/// An update keeps the maintainer's hygiene: the version update itself does not
/// remove the obsolete constructs the recipe carries.
#[test]
fn updates_never_remove_hygiene_by_the_conventions() {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let all = flat(&all_conventions());
    let system = flat(&static_system_prompt());
    for text in [&all, &system] {
        assert!(!text.contains("drop them in the same edit"), "{text}");
        assert!(
            !text.contains("Clean up obsolete spec constructs"),
            "{text}"
        );
    }
    assert!(all.contains(
        "An existing spec keeps them on an update (convention 86), unless a build or rpmlint"
    ));
    assert!(all.contains("write no bullet for a cosmetic edit, and make none"));
    let index_86 = CONVENTIONS_INDEX.lines().nth(85).unwrap();
    assert!(
        index_86.contains("only what the new release forces"),
        "{index_86}"
    );
}

const CARRIED: &str = "This spec is the package's existing recipe carried forward from 1.0 to 1.1: \
     only `Version:` and the source changed.";

/// A carried-forward spec is reviewed with the update checklist whatever its
/// Context holds, so a note about the submodules does not change the checklist.
#[test]
fn a_carried_spec_is_reviewed_with_the_update_checklist() {
    for is_python in [true, false] {
        for context in [None, Some(CARRIED), Some("## Submodules: three are empty")] {
            let ws = std::path::Path::new("/nonexistent-workspace");
            let mut req = request(ws, &[]);
            req.is_python = is_python;
            req.carried = true;
            req.context = context;
            let prompt = build_turn_prompt(&req);

            for want in [
                "## Allowed edits",
                "## Do not touch",
                "`Group:`",
                "`BuildRoot:`",
                "`%defattr`",
                "`%ldconfig_scriptlets`",
                "The glob style of `%files`",
                "`no_change`",
            ] {
                assert!(prompt.contains(want), "update prompt lacks {want:?}");
            }
            for banned in [
                "Review the generated spec",
                "# no upstream test suite",
                "Write a real one now",
                "CRITICAL BUGS FIRST",
            ] {
                assert!(!prompt.contains(banned), "update prompt has {banned:?}");
            }
        }
    }
}

#[test]
fn a_new_spec_keeps_the_full_checklist() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let python = build_turn_prompt(&request(ws, &[]));
    assert!(python.contains("Review the generated spec before the first build."));
    assert!(python.contains("CRITICAL BUGS FIRST"));
    assert!(python.contains("**%description**"));
    assert!(!python.contains("## Do not touch"));

    let mut req = request(ws, &[]);
    req.is_python = false;
    let plain = build_turn_prompt(&req);
    assert!(plain.contains("# no upstream test suite"));
    assert!(!plain.contains("## Do not touch"));
}
