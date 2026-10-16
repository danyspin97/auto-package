//! Machine-checkable conditions on what a package ships.
//!
//! A dependent that asks for a package to be patched ("`torch/utils/benchmark/utils`
//! must be in the main package, not `-devel`") can attach a [`FilesCondition`] to the
//! request. The pipeline checks it against the built RPMs' file lists, or the
//! spec's `%files` sections when nothing is built, so a request that is already
//! true is not made again, and one that a patch did not make true is known to
//! have failed, instead of being asked for again in other words.

use serde::Deserialize;

/// Where a path must be shipped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FilesCondition {
    /// The path as installed under any prefix, without a leading slash
    /// (`torch/utils/benchmark/utils`): a file or a directory.
    pub path: String,
    /// The package that must ship it: a subpackage name, or leave out for any
    /// package that is not excluded by `not_in`.
    #[serde(default)]
    pub package: Option<String>,
    /// Name fragments of packages that must NOT ship it (default `devel`).
    #[serde(default)]
    pub not_in: Vec<String>,
}

impl FilesCondition {
    /// The forbidden fragments, `devel` when none were given.
    fn forbidden(&self) -> Vec<String> {
        if self.not_in.is_empty() {
            vec!["devel".to_string()]
        } else {
            self.not_in.iter().map(|s| s.to_lowercase()).collect()
        }
    }

    /// Whether the condition can be checked at all: a plain relative path.
    pub fn is_valid(&self) -> bool {
        let p = self.path.trim_matches('/');
        !p.is_empty()
            && !p.starts_with('.')
            && !p.split('/').any(|c| c.is_empty() || c == "..")
            && !p.contains(['*', '?', '%', ' '])
    }

    /// A readable form for prompts and notes.
    pub fn describe(&self) -> String {
        let target = match &self.package {
            Some(p) => format!("package {p}"),
            None => "a package that is not excluded below".to_string(),
        };
        format!(
            "`{}` must be shipped by {target}, and not by a package whose name contains {}",
            self.path.trim_matches('/'),
            self.forbidden()
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(" or ")
        )
    }
}

/// What one package of the project ships: its name and its file entries (a full
/// path in an RPM's file list, a pattern in a spec's `%files`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFiles {
    /// `""` for the main package of a spec.
    pub name: String,
    pub files: Vec<String>,
    /// `%exclude` entries (spec only).
    pub excludes: Vec<String>,
    /// Entries are spec patterns (macros, globs), not installed paths.
    pub from_spec: bool,
}

/// The result of checking a condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Satisfied,
    /// Not true, and why.
    Violated(String),
    /// Nothing to check it against.
    Unknown,
}

/// A name without a Python flavor prefix, case and separators: `python313-Torch`
/// and `python-torch` are the same package.
fn norm(name: &str) -> String {
    let lower = name.to_lowercase();
    let rest = lower.strip_prefix("python").map_or(lower.as_str(), |r| {
        let digits = r.trim_start_matches(|c: char| c.is_ascii_digit());
        digits.strip_prefix('-').unwrap_or(&lower)
    });
    rest.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// A file entry without its `%attr(...)`, `%dir`, `%doc` words and a leading
/// macro (`%{python_sitearch}`), and without the leading slash.
fn entry_path(entry: &str) -> String {
    let mut token = entry.trim();
    loop {
        let Some(rest) = token.strip_prefix('%') else {
            break;
        };
        // A leading macro that is the start of the path (`%{python_sitearch}/x`).
        if rest.starts_with('{') {
            let end = rest.find('}').map_or(rest.len(), |e| e + 1);
            let after = &rest[end..];
            if after.starts_with('/') || after.is_empty() {
                token = after;
            } else {
                // `%{x}word`: part of a name, keep it
                break;
            }
            continue;
        }
        // `%attr(644,root,root) path`, `%dir path`, `%doc path`
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        if word.contains('/') {
            break;
        }
        token = rest[word_end..].trim_start();
    }
    token.trim_start_matches('/').trim_end_matches('/').to_string()
}

fn owns_installed(file: &str, path: &str) -> bool {
    let f = file.trim_end_matches('/');
    f.ends_with(&format!("/{path}")) || f.contains(&format!("/{path}/"))
}

fn owns_pattern(entry: &str, path: &str) -> bool {
    let e = entry_path(entry);
    if e.is_empty() {
        return false;
    }
    // A glob owns what is under the directory before its first `*`.
    let base = match e.find(['*', '?', '[']) {
        Some(i) => e[..i].trim_end_matches('/').to_string(),
        None => e.clone(),
    };
    let globbed = base != e;
    let covers = |dir: &str| {
        // The entry is the path, a directory above it, or a glob under it.
        dir == path || path.starts_with(&format!("{dir}/")) || dir.starts_with(&format!("{path}/"))
    };
    if base.is_empty() {
        return false;
    }
    // Entries carry the install prefix (`usr/lib64/python3.13/site-packages/`)
    // or a macro that was stripped: compare the tail of the entry.
    let tail_covers = |dir: &str| {
        let segs: Vec<&str> = dir.split('/').collect();
        (0..segs.len()).any(|i| covers(&segs[i..].join("/")))
    };
    if globbed {
        // `x/y/*` owns everything under `x/y`, so `x/y/z` is covered by it.
        tail_covers(&base)
    } else {
        tail_covers(&e)
    }
}

/// Whether `pkg` owns `path` (`!`excluded entries do not count).
fn owns(pkg: &PackageFiles, path: &str) -> bool {
    if pkg.from_spec {
        pkg.files.iter().any(|f| owns_pattern(f, path))
            && !pkg.excludes.iter().any(|f| owns_pattern(f, path))
    } else {
        pkg.files.iter().any(|f| owns_installed(f, path))
    }
}

/// Check `cond` against what the packages ship. `main` is the name of the
/// package the condition is about, which a spec's main package (`""`) stands for.
pub fn check(cond: &FilesCondition, packages: &[PackageFiles], main: &str) -> Verdict {
    if !cond.is_valid() {
        return Verdict::Unknown;
    }
    if packages.is_empty() {
        return Verdict::Unknown;
    }
    let path = cond.path.trim_matches('/');
    let forbidden = cond.forbidden();
    let display = |p: &PackageFiles| {
        if p.name.is_empty() {
            main.to_string()
        } else {
            p.name.clone()
        }
    };
    let owners: Vec<&PackageFiles> = packages.iter().filter(|p| owns(p, path)).collect();
    let bad: Vec<String> = owners
        .iter()
        .map(|p| display(p))
        .filter(|n| {
            let l = n.to_lowercase();
            forbidden.iter().any(|f| l.contains(f))
        })
        .collect();
    if !bad.is_empty() {
        return Verdict::Violated(format!("{path} is shipped by {}", bad.join(", ")));
    }
    let target_ok = |p: &&PackageFiles| match &cond.package {
        None => true,
        Some(want) => {
            let name = display(p);
            let (w, n) = (norm(want), norm(&name));
            // A spec's `%files devel` is `devel` here; the wanted package is the
            // name alone, or `<main>-<suffix>`.
            n == w || (p.name.is_empty() && w == norm(main))
        }
    };
    if owners.iter().any(target_ok) {
        Verdict::Satisfied
    } else if owners.is_empty() {
        Verdict::Violated(format!("no package ships {path}"))
    } else {
        Verdict::Violated(format!(
            "{path} is shipped by {}, not by {}",
            owners.iter().map(|p| display(p)).collect::<Vec<_>>().join(", "),
            cond.package.as_deref().unwrap_or("a package that is allowed")
        ))
    }
}

/// The packages a spec's `%files` sections describe, from the text alone.
pub fn spec_packages(spec: &str) -> Vec<PackageFiles> {
    let mut out: Vec<PackageFiles> = Vec::new();
    let mut current: Option<usize> = None;
    for line in spec.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("%files") {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                let args = rest.trim();
                // `%files -n name`, `%files devel`, `%files %{python_files devel}`
                let args = args
                    .strip_prefix("-n")
                    .map_or(args, |r| r.trim())
                    .replace("%{python_files", "")
                    .replace('}', "");
                let name = args.split_whitespace().next().unwrap_or("").trim().to_string();
                let name = if name.starts_with('-') { String::new() } else { name };
                let idx = out.iter().position(|p| p.name == name).unwrap_or_else(|| {
                    out.push(PackageFiles {
                        name,
                        files: Vec::new(),
                        excludes: Vec::new(),
                        from_spec: true,
                    });
                    out.len() - 1
                });
                current = Some(idx);
                continue;
            }
        }
        if t.starts_with('%') && !t.starts_with("%exclude") {
            let first = t.split_whitespace().next().unwrap_or("");
            let section = [
                "%changelog", "%prep", "%build", "%install", "%check", "%package", "%description",
                "%pre", "%post", "%preun", "%postun", "%generate_buildrequires",
            ];
            if section.contains(&first) {
                current = None;
                continue;
            }
        }
        let Some(i) = current else { continue };
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("%exclude") {
            out[i].excludes.push(rest.trim().to_string());
        } else {
            out[i].files.push(t.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(path: &str, package: Option<&str>) -> FilesCondition {
        FilesCondition {
            path: path.into(),
            package: package.map(String::from),
            not_in: Vec::new(),
        }
    }

    fn rpm(name: &str, files: &[&str]) -> PackageFiles {
        PackageFiles {
            name: name.into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            excludes: Vec::new(),
            from_spec: false,
        }
    }

    const SP: &str = "/usr/lib64/python3.13/site-packages";

    #[test]
    fn a_path_in_the_main_package_and_not_devel_is_satisfied() {
        let pkgs = [
            rpm("python313-torch", &[&format!("{SP}/torch/utils/benchmark/utils/timer.py")]),
            rpm("python313-torch-devel", &[&format!("{SP}/torch/include/x.h")]),
        ];
        let c = cond("torch/utils/benchmark/utils", Some("python-torch"));
        assert_eq!(check(&c, &pkgs, "python-torch"), Verdict::Satisfied);
    }

    #[test]
    fn the_devel_split_is_a_violation_with_the_package_named() {
        let pkgs = [
            rpm("python313-torch", &[&format!("{SP}/torch/__init__.py")]),
            rpm("python313-torch-devel", &[&format!("{SP}/torch/utils/benchmark/utils/timer.py")]),
        ];
        let c = cond("torch/utils/benchmark/utils", None);
        match check(&c, &pkgs, "python-torch") {
            Verdict::Violated(why) => assert!(why.contains("python313-torch-devel"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Absent from every package.
        let none = [rpm("python313-torch", &[&format!("{SP}/torch/__init__.py")])];
        assert!(matches!(check(&c, &none, "python-torch"), Verdict::Violated(w) if w.contains("no package ships")));
        // Nothing to check against.
        assert_eq!(check(&c, &[], "python-torch"), Verdict::Unknown);
        // A prefix lookalike is not the path.
        let look = [rpm("python313-torch", &[&format!("{SP}/torch/utils/benchmark/utils2/a.py")])];
        assert!(matches!(check(&c, &look, "python-torch"), Verdict::Violated(_)));
    }

    #[test]
    fn a_spec_files_section_is_read_per_subpackage_with_excludes() {
        let spec = "Name: python-torch\n%files %{python_files}\n%{python_sitearch}/torch\n%exclude %{python_sitearch}/torch/include\n%files %{python_files devel}\n%{python_sitearch}/torch/include\n%changelog\n- x\n";
        let pkgs = spec_packages(spec);
        assert_eq!(pkgs.len(), 2, "{pkgs:?}");
        assert_eq!(pkgs[1].name, "devel");
        let main = cond("torch/utils/benchmark/utils", Some("python-torch"));
        assert_eq!(check(&main, &pkgs, "python-torch"), Verdict::Satisfied);
        // The excluded include tree belongs to devel, which the condition forbids.
        let inc = cond("torch/include", None);
        assert!(matches!(check(&inc, &pkgs, "python-torch"), Verdict::Violated(w) if w.contains("devel")));
    }

    #[test]
    fn spec_glob_and_attr_entries_are_understood() {
        let spec = "%files\n%dir %{python_sitearch}/torch\n%attr(755,root,root) %{python_sitearch}/torch/utils/benchmark/utils/*\n";
        let pkgs = spec_packages(spec);
        assert_eq!(
            check(&cond("torch/utils/benchmark/utils", None), &pkgs, "python-torch"),
            Verdict::Satisfied
        );
        assert!(!cond("../x", None).is_valid());
        assert!(!cond("a/*", None).is_valid());
        assert!(!cond("", None).is_valid());
        assert_eq!(check(&cond("a/*", None), &pkgs, "x"), Verdict::Unknown);
    }

    #[test]
    fn the_condition_reads_from_json_with_defaults() {
        let c: FilesCondition = serde_json::from_str(r#"{"path":"a/b"}"#).unwrap();
        assert_eq!(c, cond("a/b", None));
        assert!(c.describe().contains("`devel`"));
    }
}
