//! The licence guard facts and the text of changes and changes bullets.

use super::*;

/// The lines of a recipe that say what licence its build has: the `License:`
/// tags and the `./configure` switches that make a FFmpeg GPL or non-free
/// (`--enable-gpl`, `--enable-version3`, `--enable-nonfree`), with their line
/// numbers. `%if` conditions are not evaluated: the reader sees the raw lines.
pub(super) fn recipe_licence_lines(spec: &str) -> Vec<String> {
    spec.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with('#')
                && (t.starts_with("License:")
                    || t.contains("--enable-gpl")
                    || t.contains("--enable-nonfree")
                    || t.contains("--enable-version3")
                    || t.starts_with("%if") && (t.contains("gpl") || t.contains("nonfree")))
        })
        .map(|(i, l)| format!("  line {}: {}", i + 1, l.trim()))
        .collect()
}

/// For a package whose build log shows an upstream licence guard
/// (`...LICENSE_VIOLATION`) and whose spec builds against an `ffmpeg-<N>` recipe:
/// that recipe's licence lines from this project, else from a project it builds
/// against, as the evidence convention 5 (section 78) asks for. `None` when there is no
/// guard or no recipe could be read. Cached per recipe for the process.
pub(super) fn licence_guard_facts(
    config: &PipelineConfig,
    obs_name: &str,
    build_log: &Path,
    checkout_dir: &Path,
) -> Option<String> {
    let guard = |text: &str| text.contains("LICENSE_VIOLATION");
    let log = std::fs::read_to_string(build_log).unwrap_or_default();
    let spec =
        std::fs::read_to_string(checkout_dir.join(format!("{obs_name}.spec"))).unwrap_or_default();
    if !guard(&log) && !guard(&spec) {
        return None;
    }
    let mut majors: Vec<String> = Vec::new();
    for word in spec.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if let Some(n) = word.strip_prefix("ffmpeg-") {
            let n: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !n.is_empty() && !majors.contains(&n) {
                majors.push(n);
            }
        }
    }
    if majors.is_empty() {
        return None;
    }
    static CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, Option<String>>>> =
        std::sync::Mutex::new(None);
    let emit = Emit::quiet();
    let mut projects = vec![config.project.clone()];
    if let Ok(paths) = osc::get_project_repo_paths(&config.project, emit) {
        for (p, _) in paths {
            if !projects.contains(&p) {
                projects.push(p);
            }
        }
    }
    let mut blocks: Vec<String> = Vec::new();
    for n in majors {
        let pkg = format!("ffmpeg-{n}");
        let key = format!("{}/{pkg}", config.project);
        let cached = CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|c| c.get(&key).cloned());
        let block = match cached {
            Some(b) => b,
            None => {
                let found = projects.iter().find_map(|proj| {
                    let text = crate::update::spec_for_package(proj, &pkg, emit).ok()?;
                    let lines = recipe_licence_lines(&text);
                    (!lines.is_empty()).then(|| {
                        format!(
                            "The recipe of `{pkg}` as {proj} has it:\n{}",
                            lines.join("\n")
                        )
                    })
                });
                // `spec_for_package` fails both when the recipe is absent and
                // when an osc call failed transiently, and the two are not
                // told apart here. Only a found recipe is cached: a `None`
                // cached for the whole process would hide the recipe for
                // every later guard in this run.
                if found.is_some() {
                    CACHE
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert_with(Default::default)
                        .insert(key, found.clone());
                }
                found
            }
        };
        blocks.extend(block);
    }
    (!blocks.is_empty()).then(|| {
        format!(
            "## Licence facts (read by the pipeline)\n\nThis package's build hit an upstream \
             licence guard. These are the licence lines of the FFmpeg recipe it builds \
             against, read from OBS for you: this is the evidence convention 5 (section 78) asks you to \
             check (no `--enable-gpl`/`--enable-nonfree`, an LGPL `License:` means the check \
             holds; a `%if` line may switch a switch off, so read it with its condition). \
             Cite it in the `# licence-guard-verified:` comment.\n\n{}",
            blocks.join("\n\n")
        )
    })
}

/// The `.changes` bullets for an agent turn: one bullet per line of `text` (the
/// changelog field, or the reasoning flattened to one line), each on a single
/// line of at most 120 characters. A line that only narrates becomes the
/// generic bullet.
pub(super) fn changes_message(text: &str) -> String {
    let bullets: Vec<String> = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
        .map(change_bullet)
        .filter(|b| !b.is_empty())
        .map(|b| {
            if is_narration(&b) {
                "Fix the packaging".to_string()
            } else {
                b
            }
        })
        .collect();
    if bullets.is_empty() {
        "Update the packaging".to_string()
    } else {
        bullets.join("\n")
    }
}

/// One bullet from one line: its first sentence, on one line, cut to 120
/// characters. Empty when the line has no words.
fn change_bullet(reasoning: &str) -> String {
    let flat = reasoning.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = flat
        .split_once(". ")
        .map(|(s, _)| s)
        .unwrap_or(&flat)
        .trim_end_matches('.');
    // `.changes` is plain text, not a spec: a doubled `%%` is a spec escape.
    let first = first.replace("%%", "%");
    let first = without_file_list(&first);
    // A changelog line is read at a glance: past 120 characters it is cut at a
    // clause boundary (or at a word when there is none), with no ellipsis.
    let line: String = if first.chars().count() > 120 {
        let cut: String = first.chars().take(120).collect();
        // Boundary candidates must leave a meaningful head.
        let clause = [", ", "; ", ": ", " - ", " — ", " ("]
            .iter()
            .filter_map(|b| cut.rfind(b))
            .filter(|&i| cut[..i].chars().count() >= 40)
            .max();
        let head = match clause {
            Some(i) => &cut[..i],
            None => cut.rsplit_once(' ').map(|(h, _)| h).unwrap_or(&cut),
        };
        head.trim_end_matches([',', ';', ':', ' ', '-', '('])
            .to_string()
    } else {
        first.to_string()
    };
    line
}

/// Whether a bullet narrates the investigation (what the build log showed, what
/// was reviewed) instead of saying what changed in the package.
pub(super) fn is_narration(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    [
        "the",
        "all",
        "this",
        "it",
        "pre-build",
        "reviewed",
        "review",
        "restored",
    ]
    .contains(&first)
        || lower.contains("build.log")
        || lower.contains("build log")
        || lower.contains("despite")
}

/// `line` with the package's own macros written out. A `.changes` entry is
/// plain text that rpm later reads as `%changelog`, where `%{name}` would be
/// expanded or flagged, so `lib%{name}1_4` becomes `libaws-c-s31_4`.
pub(super) fn without_macros(line: &str, package: &str, version: &str) -> String {
    line.replace("%{name}", package)
        .replace("%name", package)
        .replace("%{version}", version)
        .replace("%version", version)
}

/// `line` without a trailing parenthesis that lists files or paths
/// (`... already contains (src/fa.c, fa.h, tests/fatest.c)`): a changelog says
/// what changed, not where.
pub(super) fn without_file_list(line: &str) -> String {
    let Some(head) = line.trim_end().strip_suffix(')') else {
        return line.to_string();
    };
    let Some((before, inside)) = head.rsplit_once(" (") else {
        return line.to_string();
    };
    let looks_like_files = |item: &str| {
        let item = item.trim().trim_matches('`');
        !item.is_empty()
            && !item.contains(' ')
            && (item.contains('/')
                || item.rsplit_once('.').is_some_and(|(_, e)| {
                    (1..=4).contains(&e.len()) && e.chars().any(|c| c.is_ascii_alphabetic())
                }))
    };
    if before.trim().is_empty() || !inside.split(',').all(looks_like_files) {
        return line.to_string();
    }
    before.trim_end().to_string()
}

/// `line` without a trailing "in/for/of <package> <version>" naming the release
/// this run packages: the `- Update to <version>` entry just below says which
/// release that is. Another package's version, or one mid-sentence, stays.
pub(super) fn without_own_version(line: &str, package: &str, version: &str) -> String {
    let Some(head) = line.strip_suffix(version) else {
        return line.to_string();
    };
    if !head.ends_with(' ') {
        return line.to_string();
    }
    let mut words: Vec<&str> = head.split_whitespace().collect();
    let mut stripped = false;
    if words
        .last()
        .is_some_and(|w| w.eq_ignore_ascii_case(package))
    {
        words.pop();
        stripped = true;
    }
    if stripped
        && words
            .last()
            .is_some_and(|w| ["in", "for", "of", "with", "from", "on", "against", "to"].contains(w))
    {
        words.pop();
    }
    // Only a "<package> <version>" ending is the release's own name; a bare
    // trailing number ("Raise the minimum to 1.2") may mean something else.
    if !stripped || words.is_empty() {
        return line.to_string();
    }
    words.join(" ")
}

/// Count added/removed lines in a diff produced by [`unified_diff`].
pub(super) fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(a, r), l| {
        if l.starts_with("+ ") {
            (a + 1, r)
        } else if l.starts_with("- ") {
            (a, r + 1)
        } else {
            (a, r)
        }
    })
}
