//! Relaxing the dependency bounds of a spec and adding build requirements.

/// Parse `op version` (`>= 0.1.10`, `< 0.1.12`, `= 2.0`) of one RPM dependency
/// item and say how `shipped` stands against it: `Some(None)` when it meets it,
/// `Some(Some(true))` when it is too new, `Some(Some(false))` when too old.
/// `None` when it is not a constraint this reads.
pub(crate) fn rpm_constraint_miss(
    constraint: &str,
    shipped: &pep440_rs::Version,
) -> Option<Option<bool>> {
    use std::str::FromStr;
    let c = constraint.trim();
    let (op, rest) = ["<=", ">=", "==", "<", ">", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim())))?;
    // An RPM version may carry an epoch or a release: only the upstream part.
    let ver = rest.split(['-', ' ']).next().unwrap_or(rest);
    let ver = ver.rsplit_once(':').map(|(_, v)| v).unwrap_or(ver);
    let wanted = pep440_rs::Version::from_str(ver).ok()?;
    let met = match op {
        "<=" => shipped <= &wanted,
        ">=" => shipped >= &wanted,
        "<" => shipped < &wanted,
        ">" => shipped > &wanted,
        _ => shipped == &wanted,
    };
    if met {
        return Some(None);
    }
    Some(Some(match op {
        "<=" | "<" => true,
        ">=" | ">" => false,
        _ => shipped > &wanted,
    }))
}

/// What relaxing does to one constraint the shipped version is checked against.
enum Relax {
    /// The shipped version meets it, or it cannot be relaxed: a minimum, or a pin
    /// above the shipped version, stays as it is.
    Keep,
    /// An upper bound the shipped version exceeds: removed.
    Drop,
    /// An exact pin below the shipped version: it becomes this minimum.
    Minimum(String),
}

/// How `constraint` (`< 0.34`, `== 2.11.1`) is relaxed for the version `shipped`
/// (convention 75, sections 59 and 73): an upper bound the shipped version exceeds goes, an
/// exact pin below it becomes `>= <pin>`, and everything else stays, a minimum the
/// shipped version does not reach included (relaxing it would not make the package
/// resolvable).
fn relax_constraint(constraint: &str, shipped: &pep440_rs::Version) -> Relax {
    let c = constraint.trim();
    let Some((op, rest)) = ["<=", ">=", "==", "<", ">", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim())))
    else {
        return Relax::Keep;
    };
    match (op, rpm_constraint_miss(c, shipped)) {
        ("<" | "<=", Some(Some(true))) => Relax::Drop,
        ("=" | "==", Some(Some(true))) => Relax::Minimum(format!(">= {rest}")),
        _ => Relax::Keep,
    }
}

/// The dependency lines of `spec` for `dependency` relaxed for the version
/// `shipped` (see [`relax_constraint`]): an upper bound it exceeds is removed and
/// an exact pin below it becomes `>= <pin>`, the name kept, so a package the distro
/// ships is used as it is (convention 75, section 59). A minimum it does not reach, and
/// every constraint it meets, stay. Each change gets an `# upstream pins` comment.
/// Duplicate lines this leaves are merged.
pub fn relax_dependency_bounds(spec: &str, dependency: &str, shipped: &str) -> String {
    use std::str::FromStr;
    let Ok(shipped_v) = pep440_rs::Version::from_str(shipped.trim()) else {
        return spec.to_string();
    };
    let wanted = crate::osc::fuzzy_key(dependency);
    let mut out: Vec<String> = Vec::new();
    // One dedupe set per conditional branch: the same text in the `%if` and the
    // `%else` branch is two different requirements, not a duplicate.
    let mut seen_stack: Vec<std::collections::HashSet<String>> = vec![Default::default()];
    const OPEN_CONDITIONALS: &[&str] = &["%if", "%ifarch", "%ifnarch", "%ifos", "%ifnos"];
    const BRANCH_CONDITIONALS: &[&str] = &[
        "%elif",
        "%elifarch",
        "%elifnarch",
        "%elifos",
        "%elifnos",
        "%else",
    ];
    for line in spec.lines() {
        let t = line.trim_start();
        let keyword = t.split_whitespace().next().unwrap_or_default();
        if OPEN_CONDITIONALS.contains(&keyword) {
            seen_stack.push(Default::default());
            out.push(line.to_string());
            continue;
        }
        if BRANCH_CONDITIONALS.contains(&keyword) {
            if let Some(top) = seen_stack.last_mut() {
                top.clear();
            }
            out.push(line.to_string());
            continue;
        }
        if keyword == "%endif" {
            if seen_stack.len() > 1 {
                seen_stack.pop();
            }
            out.push(line.to_string());
            continue;
        }
        let is_dep_line = ["BuildRequires", "Requires", "Recommends", "Suggests"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if !is_dep_line {
            out.push(line.to_string());
            continue;
        }
        let (head, value) = line.split_once(':').unwrap_or((line, ""));
        // The bounds this line loses, as `(name, constraint)`: each gets the
        // `# upstream pins` comment above the line (convention 75).
        let mut dropped: Vec<(String, String)> = Vec::new();
        let compact = |c: &str| c.split_whitespace().collect::<String>();
        let short_name = |name: &str| {
            name.strip_prefix("python-")
                .or_else(|| name.strip_prefix("python3-"))
                .unwrap_or(name)
                .to_string()
        };
        let items: Vec<String> = value
            .split(',')
            .map(|item| {
                let item = item.trim();
                // `%{python_module name >= v}` or `python-name >= v`.
                let (open, inner, close) = match item
                    .strip_prefix("%{python_module ")
                    .and_then(|r| r.strip_suffix('}'))
                {
                    Some(inner) => ("%{python_module ", inner, "}"),
                    None => ("", item, ""),
                };
                let name_end = inner
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '='))
                    .unwrap_or(inner.len());
                let name = &inner[..name_end];
                let key = crate::osc::fuzzy_key(
                    name.strip_prefix("python-")
                        .or_else(|| name.strip_prefix("python3-"))
                        .unwrap_or(name),
                );
                let key_plain = crate::osc::fuzzy_key(name);
                if key != wanted && key_plain != wanted {
                    return item.to_string();
                }
                let constraint = inner[name_end..].trim();
                if constraint.is_empty() || constraint.contains(" if ") {
                    return item.to_string();
                }
                // `%{python_module trio >= 0.25.1 with %python-trio < 0.34}`:
                // each clause is judged on its own, the ones met stay.
                if !open.is_empty() && constraint.contains(" with ") {
                    let clauses: Vec<String> = constraint
                        .split(" with ")
                        .map(|c| {
                            c.trim_start_matches(|ch: char| !matches!(ch, '<' | '>' | '='))
                                .trim()
                                .to_string()
                        })
                        .collect();
                    let mut kept: Vec<String> = Vec::new();
                    for c in &clauses {
                        match relax_constraint(c, &shipped_v) {
                            Relax::Keep => kept.push(c.clone()),
                            Relax::Drop => dropped.push((short_name(name), compact(c))),
                            Relax::Minimum(min) => {
                                dropped.push((short_name(name), compact(c)));
                                kept.push(min);
                            }
                        }
                    }
                    return if kept == clauses {
                        item.to_string()
                    } else if kept.is_empty() {
                        format!("{open}{name}{close}")
                    } else {
                        format!(
                            "{open}{name} {}{close}",
                            kept.join(&format!(" with %python-{name} "))
                        )
                    };
                }
                match relax_constraint(constraint, &shipped_v) {
                    Relax::Keep => item.to_string(),
                    Relax::Drop => {
                        dropped.push((short_name(name), compact(constraint)));
                        format!("{open}{name}{close}")
                    }
                    Relax::Minimum(min) => {
                        dropped.push((short_name(name), compact(constraint)));
                        format!("{open}{name} {min}{close}")
                    }
                }
            })
            .collect();
        let rebuilt = format!(
            "{head}:{}{}",
            if value.starts_with(' ') || value.starts_with('\t') {
                &value[..value.len() - value.trim_start().len()]
            } else {
                " "
            },
            items.join(", ")
        );
        // A line that reads the same as one already kept in this branch is not
        // repeated.
        let seen = seen_stack
            .last_mut()
            .expect("the base scope is never popped");
        if rebuilt != line && !seen.insert(rebuilt.trim().to_string()) {
            continue;
        }
        seen.insert(rebuilt.trim().to_string());
        for (name, constraint) in dropped {
            // A `%` would be expanded by rpm inside a comment.
            let note = format!(
                "# upstream pins {name}{constraint}; relaxed because the shipped {} does not satisfy it",
                shipped.trim()
            )
            .replace('%', "");
            if out.last() != Some(&note) {
                out.push(note);
            }
        }
        out.push(rebuilt);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// [`relax_dependency_bounds`] for each `(dependency, shipped version)` a
/// package's state records (`PkgState::relaxed_dependencies`): applied on every
/// validation, root package included, before anything is built.
pub fn relax_dependencies(spec: &str, relaxed: &[(String, String)]) -> String {
    relaxed
        .iter()
        .fold(spec.to_string(), |spec, (dep, shipped)| {
            relax_dependency_bounds(&spec, dep, shipped)
        })
}

/// Whether `requirement` is a package a spec can name: `%{python_module ...}`
/// around it is allowed, and so is a version constraint after the name. The
/// name must match `^[A-Za-z0-9][A-Za-z0-9._+-]*$`; the constraint may hold only
/// letters, digits, spaces and `. _ + - < > = ! ~ , *`, so no macro, shell
/// or spec character reaches the line.
fn is_plain_build_requirement(requirement: &str) -> bool {
    let inner = requirement
        .strip_prefix("%{python_module ")
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(requirement)
        .trim();
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')))
        .unwrap_or(inner.len());
    let (name, constraint) = inner.split_at(name_end);
    name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && constraint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._+-<>=!~,*".contains(c))
}

/// `spec` with `BuildRequires:  <requirement>` added after the last
/// `BuildRequires:` line, or `None` when it is already there, or the name is
/// not plain (a warning is logged). Used to settle a provider the solver cannot
/// choose between.
pub fn add_build_requires(spec: &str, requirement: &str) -> Option<String> {
    let wanted = requirement.trim();
    if !is_plain_build_requirement(wanted) {
        tracing::warn!("not adding BuildRequires {wanted:?}: not a plain package name");
        return None;
    }
    let has = spec.lines().any(|l| {
        l.trim_start()
            .strip_prefix("BuildRequires:")
            .is_some_and(|v| v.trim() == wanted)
    });
    if has {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("BuildRequires:"))?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at + 1, format!("BuildRequires:  {wanted}"));
    Some(out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}
