//! Whole-file passes that bring a spec in line with the packaging style.
//!
//! Each pass is deterministic and idempotent, so the validate step re-applies
//! all of them to a spec on every visit (see `validate_round`). They exist
//! because a review of the specs of a full run found the same slips on dozens
//! of packages: a model asked to fix them one by one spends a turn on each.

/// Headers that start a new section of a spec.
const SECTION_HEADERS: &[&str] = &[
    "%prep",
    "%build",
    "%install",
    "%check",
    "%conf",
    "%files",
    "%changelog",
    "%description",
    "%package",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%verifyscript",
    "%generate_buildrequires",
    "%ldconfig_scriptlets",
];

/// Headers whose section is a scriptlet or the file list: `%check` belongs
/// before the first of them.
const AFTER_CHECK: &[&str] = &[
    "%files",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%verifyscript",
    "%ldconfig_scriptlets",
];

fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

fn is_section(line: &str) -> bool {
    SECTION_HEADERS.contains(&first_word(line))
}

fn join(lines: &[&str], trailing_newline: bool) -> String {
    lines.join("\n") + if trailing_newline { "\n" } else { "" }
}

/// `%check` moved up to sit before the scriptlets and `%files`, with the
/// comment lines that sit right above it. A `%check` that follows `%files`
/// works, but the usual order is `%install`, `%check`, scriptlets, `%files`.
pub fn move_check_before_files(spec: &str) -> String {
    let lines: Vec<&str> = spec.lines().collect();
    let Some(check) = lines.iter().position(|l| l.trim_end() == "%check") else {
        return spec.to_string();
    };
    let Some(files) = lines.iter().position(|l| first_word(l) == "%files") else {
        return spec.to_string();
    };
    if check < files {
        return spec.to_string();
    }
    let end = lines[check + 1..]
        .iter()
        .position(|l| is_section(l))
        .map_or(lines.len(), |p| check + 1 + p);
    let mut start = check;
    while start > 0 && lines[start - 1].trim_start().starts_with('#') {
        start -= 1;
    }
    // A `%check` split by a conditional that opens or closes outside it is left.
    let opens = lines[start..end]
        .iter()
        .filter(|l| first_word(l).starts_with("%if"))
        .count();
    let closes = lines[start..end]
        .iter()
        .filter(|l| first_word(l) == "%endif")
        .count();
    if opens != closes {
        return spec.to_string();
    }
    let install = lines
        .iter()
        .position(|l| first_word(l) == "%install")
        .unwrap_or(0);
    // A `%check` above `%install` (or above the scriptlets it would move into)
    // is out of the usual order: leave the spec as it is rather than guess.
    if install >= start {
        return spec.to_string();
    }
    let Some(anchor) = lines[install..start]
        .iter()
        .position(|l| AFTER_CHECK.contains(&first_word(l)))
        .map(|p| install + p)
    else {
        return spec.to_string();
    };

    let trimmed = |s: &[&str]| -> usize {
        let mut n = s.len();
        while n > 0 && s[n - 1].trim().is_empty() {
            n -= 1;
        }
        n
    };

    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + 2);
    let head = &lines[..anchor];
    out.extend_from_slice(&head[..trimmed(head)]);
    out.push("");
    let block = &lines[start..end];
    out.extend_from_slice(&block[..trimmed(block)]);
    out.push("");
    let middle = &lines[anchor..start];
    out.extend_from_slice(&middle[..trimmed(middle)]);
    let rest = &lines[end..];
    let skip = rest.iter().take_while(|l| l.trim().is_empty()).count();
    if skip < rest.len() {
        out.push("");
        out.extend_from_slice(&rest[skip..]);
    }
    join(&out, spec.ends_with('\n'))
}

/// A `%{python_expand #` ... `}` wrapper around plain `%python_expand` lines
/// removed. The wrapper adds nothing, and its `#` is a comment inside a macro
/// body that rpm treats differently from one in the spec.
pub fn unwrap_python_expand_block(spec: &str) -> String {
    let lines: Vec<&str> = spec.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end() == "%{python_expand #"
            && let Some(close) = lines[i + 1..].iter().position(|l| l.trim_end() == "}")
            && lines[i + 1..i + 1 + close]
                .iter()
                .all(|l| l.starts_with("%python_expand ") || l.trim().is_empty())
        {
            out.extend_from_slice(&lines[i + 1..i + 1 + close]);
            i += close + 2;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    join(&out, spec.ends_with('\n'))
}

/// Text that only a compiled package would carry.
const COMPILED_MARKERS: &[&str] = &[
    "%{python_sitearch}",
    "%{$python_sitearch}",
    "python_module devel",
    "python_module cython",
    "python_module cffi",
    "python_module maturin",
    "python_module pybind11",
    "python_module nanobind",
    "python_module setuptools-rust",
    "cargo",
    "gcc",
    "cmake",
    "ninja",
    "meson",
    "swig",
    "%{optflags}",
    ".whl",
];

/// `BuildArch: noarch` added to a Python package that ships only pure Python:
/// it uses `%python_subpackages`, installs into `%{python_sitelib}`, compiles
/// nothing and has no architecture tag. Without it every flavor package is
/// built, and published, once per architecture.
pub fn add_noarch_to_pure_python(spec: &str) -> String {
    let lines: Vec<&str> = spec.lines().collect();
    let Some(sub) = lines.iter().position(|l| l.trim() == "%python_subpackages") else {
        return spec.to_string();
    };
    let has_tag = |tag: &str| lines.iter().any(|l| l.starts_with(tag));
    if has_tag("BuildArch:") || has_tag("ExclusiveArch:") || has_tag("ExcludeArch:") {
        return spec.to_string();
    }
    let code: String = lines
        .iter()
        .filter(|l| !l.trim_start().starts_with('#'))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if !code.contains("%{python_sitelib}") && !code.contains("%{$python_sitelib}") {
        return spec.to_string();
    }
    if COMPILED_MARKERS.iter().any(|m| code.contains(m)) {
        return spec.to_string();
    }

    // After the last top-level BuildRequires; before `%python_subpackages`
    // when there is none.
    let mut depth = 0i32;
    let mut after_br = None;
    for (i, l) in lines[..sub].iter().enumerate() {
        match first_word(l) {
            w if w.starts_with("%if") => depth += 1,
            "%endif" => depth -= 1,
            _ => {}
        }
        if depth == 0 && l.starts_with("BuildRequires:") {
            after_br = Some(i + 1);
        }
    }
    let at = after_br.unwrap_or(sub);
    let mut out = lines.clone();
    out.insert(at, "BuildArch:      noarch");
    join(&out, spec.ends_with('\n'))
}

/// Whether `s[at..at + len]` is a whole version: not the middle of a longer
/// dotted number (`2.3` in `1.2.3`, `1.2` in `1.2.3`).
fn is_whole_version(s: &str, at: usize, len: usize) -> bool {
    let b = s.as_bytes();
    if at > 0 {
        let p = b[at - 1];
        if p.is_ascii_digit() || (p == b'.' && at > 1 && b[at - 2].is_ascii_digit()) {
            return false;
        }
    }
    if let Some(&n) = b.get(at + len) {
        if n.is_ascii_digit() || (n == b'.' && b.get(at + len + 1).is_some_and(u8::is_ascii_digit))
        {
            return false;
        }
    }
    true
}

fn replace_version(line: &str, version: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = 0;
    while let Some(p) = line[rest..].find(version) {
        let at = rest + p;
        // Inside an open `%{...}` the text is an expression, not a path.
        let before = &line[..at];
        let in_macro = before
            .rfind("%{")
            .is_some_and(|o| !before[o..].contains('}'));
        out.push_str(&line[rest..at]);
        if is_whole_version(line, at, version.len()) && !in_macro {
            out.push_str("%{version}");
        } else {
            out.push_str(version);
        }
        rest = at + version.len();
    }
    out.push_str(&line[rest..]);
    out
}

/// The spec's own version written as `%{version}` in the `Source` tags, the
/// `-n` directory of `%setup`/`%autosetup`, and `%files`, where a literal copy
/// goes stale at the next update and makes the build fail on a missing file.
pub fn use_version_macro(spec: &str) -> String {
    let Some(version) = spec
        .lines()
        .find_map(|l| l.strip_prefix("Version:"))
        .map(str::trim)
    else {
        return spec.to_string();
    };
    if version.contains('%') || !version.contains('.') || version.len() < 3 {
        return spec.to_string();
    }
    let mut in_files = false;
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        if is_section(line) {
            in_files = first_word(line) == "%files";
        }
        let t = line.trim_start();
        let eligible = !t.starts_with('#')
            && !line.starts_with("Version:")
            && (in_files
                || (line.starts_with("Source") && line.contains(':'))
                || (matches!(first_word(line), "%setup" | "%autosetup") && line.contains("-n ")));
        if eligible && line.contains(version) {
            out.push(replace_version(line, version));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// The build dependencies of a spec that only unpacks a prebuilt wheel
/// (`%pyproject_install %{SOURCE0}`) dropped, except `pip`, which installs it.
/// Nothing is compiled and `%check` only looks for files, so the Python
/// modules the package requires at run time are not needed to build it, and
/// each one is another package that has to be built first.
pub fn drop_unneeded_wheel_build_requires(spec: &str) -> String {
    let installs_wheel = spec
        .lines()
        .any(|l| l.contains("pyproject_install %{SOURCE"));
    let has_check = spec.lines().any(|l| l.trim_end() == "%check");
    if !installs_wheel || (has_check && !crate::spec::check_is_file_probe_only(spec)) {
        return spec.to_string();
    }
    let keep = |l: &str| {
        !l.starts_with("BuildRequires:") || python_module_name(l).is_none_or(|name| name == "pip")
    };
    let lines: Vec<&str> = spec.lines().filter(|l| keep(l)).collect();
    join(&lines, spec.ends_with('\n'))
}

/// The module a `%{python_module <name> [<constraint>]}` macro names in `line`
/// (`pip` in `%{python_module pip >= 19}`), or `None` when the line has none.
fn python_module_name(line: &str) -> Option<&str> {
    let (_, after) = line.split_once("%{python_module ")?;
    after.split('}').next()?.split_whitespace().next()
}

/// The two-line note of the archive a spec was generated from, as comments
/// right above the first `Source` tag: after `%changelog` they read as part of
/// the changelog. A spec with no source tag gets them at the end.
pub fn with_archive_note(spec: &str, url: &str, sha256: &str) -> String {
    if spec.contains("# Source archive sha256:") {
        return spec.to_string();
    }
    // A control character (a newline above all) would end the comment and
    // start a spec line of its own; whitespace is refused too so the URL
    // stays one word. Any `%` is doubled: rpm expands macros even inside
    // comments (`%{lua:...}` runs at parse time), `%%` is a literal `%`.
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        tracing::warn!("not writing the source archive note: the URL has whitespace or control characters");
        return spec.to_string();
    }
    let url = url.replace('%', "%%");
    let note = [
        format!("# Source archive: {url}"),
        format!("# Source archive sha256: {sha256}"),
    ];
    let mut lines: Vec<String> = spec.lines().map(str::to_string).collect();
    match lines.iter().position(|l| {
        l.strip_prefix("Source").is_some_and(|r| {
            r.trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with(':')
        })
    }) {
        Some(at) => {
            for (i, n) in note.into_iter().enumerate() {
                lines.insert(at + i, n);
            }
        }
        None => {
            lines.push(String::new());
            lines.extend(note);
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_note_refuses_a_url_that_would_start_a_spec_line() {
        let spec = "Name: x\nSource0: a\n%changelog\n";
        for url in ["https://u/a\n%pre\necho x", "https://u/a b", "https://u/a\rx", "https://u/\tx"] {
            assert_eq!(with_archive_note(spec, url, "abc"), spec, "{url:?}");
        }
    }

    #[test]
    fn archive_note_doubles_percent_so_rpm_does_not_expand_it() {
        let out = with_archive_note("Name: x\nSource0: a\n%changelog\n", "https://u/a%2Fb", "abc");
        assert!(out.contains("# Source archive: https://u/a%%2Fb\n"), "{out}");
        let lua = with_archive_note("Name: x\nSource0: a\n", "https://u/%{lua:x}", "abc");
        assert!(lua.contains("# Source archive: https://u/%%{lua:x}\n"), "{lua}");
        // Every `%` left is half of a `%%`: no bare macro survives.
        assert!(!lua.replace("%%", "").contains("%{lua"), "{lua}");
        assert_eq!(lua.lines().filter(|l| l.starts_with('#')).count(), 2);
    }

    #[test]
    fn check_moves_before_the_scriptlets_and_files_with_its_comment() {
        let spec = "%install\nx\n\n%post\np\n\n%files\n/a\n\n# why\n%check\nt\n\n%changelog\n";
        assert_eq!(
            move_check_before_files(spec),
            "%install\nx\n\n# why\n%check\nt\n\n%post\np\n\n%files\n/a\n\n%changelog\n"
        );
        let again = move_check_before_files(&move_check_before_files(spec));
        assert_eq!(again, move_check_before_files(spec));
    }

    #[test]
    fn check_already_before_files_stays_and_a_trailing_check_has_no_changelog_needed() {
        let ok = "%install\nx\n\n%check\nt\n\n%files\n/a\n";
        assert_eq!(move_check_before_files(ok), ok);
        let last = "%install\nx\n\n%files\n/a\n\n%check\nt\n";
        assert_eq!(
            move_check_before_files(last),
            "%install\nx\n\n%check\nt\n\n%files\n/a\n"
        );
    }

    #[test]
    fn check_above_install_leaves_the_spec_unchanged() {
        // `%files` above `%check`, but `%check` above `%install`: the anchor
        // search used to slice `lines[install..start]` with install > start.
        let spec = "%files\n/a\n%check\nt\n%install\nx\n";
        assert_eq!(move_check_before_files(spec), spec);
    }

    #[test]
    fn files_above_prep_with_check_above_install_leaves_the_spec_unchanged() {
        let spec = "%package devel\n%description devel\nd\n%files devel\n/d\n\
                    %prep\np\n%check\nt\n%install\nx\n%files\n/a\n";
        assert_eq!(move_check_before_files(spec), spec);
    }

    #[test]
    fn the_python_expand_wrapper_goes_but_a_real_block_stays() {
        let hack = "%{python_expand #\n%python_expand test -e a\n%python_expand test -e b\n}\n";
        assert_eq!(
            unwrap_python_expand_block(hack),
            "%python_expand test -e a\n%python_expand test -e b\n"
        );
        let real = "%{python_expand export X=1\n%python_expand build\n}\n";
        assert_eq!(unwrap_python_expand_block(real), real);
    }

    const PURE: &str = "Name: python-x\nBuildRequires:  fdupes\nRequires: python-y\n%python_subpackages\n\n%files %{python_files}\n%{python_sitelib}/x/\n";

    #[test]
    fn a_pure_python_package_becomes_noarch_after_its_build_requires() {
        let out = add_noarch_to_pure_python(PURE);
        assert!(out.contains("BuildRequires:  fdupes\nBuildArch:      noarch\nRequires"));
        assert_eq!(add_noarch_to_pure_python(&out), out);
    }

    #[test]
    fn a_compiled_or_arch_tagged_package_is_not_made_noarch() {
        for spec in [
            PURE.replace("sitelib", "sitearch"),
            PURE.replace("fdupes", "%{python_module devel}"),
            PURE.replace("Requires:", "ExclusiveArch: x86_64\nRequires:"),
            PURE.replace("Requires:", "BuildArch: noarch\nRequires:"),
            PURE.replace("python-y", "python-y\nSource0: a.whl"),
        ] {
            assert!(
                !add_noarch_to_pure_python(&spec).contains("BuildArch:      noarch"),
                "{spec}"
            );
        }
    }

    #[test]
    fn the_version_is_a_macro_in_sources_files_and_setup_but_not_in_longer_numbers() {
        let spec = "Version:        1.2.3\nSource0:        https://x/a-1.2.3.tar.gz\n\
                    %prep\n%autosetup -n a-1.2.3\n%files\n/lib/liba.so.1.2.3\n/lib/b-11.2.3\n\
                    # a-1.2.3\n%changelog\n- 1.2.3\n";
        let out = use_version_macro(spec);
        assert!(out.contains("Source0:        https://x/a-%{version}.tar.gz"));
        assert!(out.contains("%autosetup -n a-%{version}"));
        assert!(out.contains("/lib/liba.so.%{version}"));
        assert!(out.contains("/lib/b-11.2.3"));
        assert!(out.contains("# a-1.2.3"));
        assert!(out.contains("- 1.2.3"));
        assert_eq!(use_version_macro(&out), out);
    }

    #[test]
    fn a_wheel_install_keeps_only_pip_as_a_python_build_dependency() {
        let spec = "BuildRequires:  %{python_module pip}\nBuildRequires:  %{python_module jinja2}\n\
                    BuildRequires:  fdupes\n%install\n%pyproject_install %{SOURCE0}\n\n\
                    %check\n%python_expand test -e %{buildroot}/x\n";
        assert_eq!(
            drop_unneeded_wheel_build_requires(spec),
            "BuildRequires:  %{python_module pip}\nBuildRequires:  fdupes\n%install\n\
             %pyproject_install %{SOURCE0}\n\n%check\n%python_expand test -e %{buildroot}/x\n"
        );
        let imports = spec.replace("test -e %{buildroot}/x", "$python -c 'import x'");
        assert_eq!(drop_unneeded_wheel_build_requires(&imports), imports);
    }

    #[test]
    fn a_pip_requirement_with_a_version_constraint_is_kept() {
        let spec = "BuildRequires:  %{python_module pip >= 19}\n\
                    BuildRequires:  %{python_module  setuptools  }\n\
                    BuildRequires:  %{python_module wheel}\n\
                    %install\n%pyproject_install %{SOURCE0}\n";
        assert_eq!(
            drop_unneeded_wheel_build_requires(spec),
            "BuildRequires:  %{python_module pip >= 19}\n%install\n%pyproject_install %{SOURCE0}\n"
        );
        assert_eq!(
            python_module_name("BuildRequires: %{python_module pip}"),
            Some("pip")
        );
        assert_eq!(python_module_name("BuildRequires: fdupes"), None);
    }

    #[test]
    fn the_archive_note_goes_above_the_first_source() {
        let out = with_archive_note("Name: x\nSource0: a\n%changelog\n", "https://u/a", "abc");
        assert_eq!(
            out,
            "Name: x\n# Source archive: https://u/a\n# Source archive sha256: abc\nSource0: a\n%changelog\n"
        );
        assert_eq!(with_archive_note(&out, "https://u/a", "abc"), out);
    }
}
