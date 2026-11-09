//! `BuildRequires: %{python_module foo >= 1.2}` as the package requirements
//! it expands to: one `<flavor>-foo >= 1.2` per Python flavor the branch
//! builds for (`python313-foo`), as the macro does. A flavored name is a
//! binary name, so it is looked up like any package name and its hit carries
//! the source package (`python-foo`) the dependency decision compares.
//!
//! Only the plain forms are read: `%{python_module name}` and
//! `%{python_module name op version}`. Anything else (a macro in the name, a
//! condition, another word count) stays the capability the spec reader gave,
//! which is reported as not checked. A macro in the version is kept: the
//! bound cannot be compared, as for a package name with one.

use crate::spec::{Bound, Requirement};

use super::*;

const BOUND_OPS: [&str; 6] = [">=", "<=", "=", "==", "<", ">"];

/// The module name and the bound of a `%{python_module ...}` capability:
/// `("foo", Some(>= 1.2))` for `%{python_module foo >= 1.2}`. `None` for any
/// other capability, and for a form whose name cannot be told.
pub fn python_module(capability: &str) -> Option<(&str, Option<Bound>)> {
    let inner = capability
        .strip_prefix("%{python_module")?
        .strip_suffix('}')?;
    if !inner.starts_with(char::is_whitespace) {
        return None;
    }
    let words: Vec<&str> = inner.split_whitespace().collect();
    let (name, bound) = match words.as_slice() {
        [name] => (*name, None),
        [name, op, version] if BOUND_OPS.contains(op) => (
            *name,
            Some(Bound {
                op: op.to_string(),
                version: version.to_string(),
            }),
        ),
        _ => return None,
    };
    let plain = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
    (plain && name.starts_with(|c: char| c.is_ascii_alphanumeric())).then_some((name, bound))
}

/// A spec's requirements with `%{python_module}` read for its flavors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonModules {
    /// The requirements, each `%{python_module}` this reads replaced by its
    /// flavored package requirements, in its place.
    pub requirements: Vec<Requirement>,
    /// The flavored names only a `%{python_module}` gave, not also written
    /// as a package name: looked up, but not reasons to add a build path.
    pub flavored: Vec<String>,
}

/// `reqs` with each `%{python_module}` [`python_module`] reads replaced by
/// one package requirement per flavor in `flavors` (`python313`), with its
/// bound. Without flavors nothing is replaced.
pub fn expand_python_modules(reqs: &[Requirement], flavors: &[String]) -> PythonModules {
    let mut requirements: Vec<Requirement> = Vec::new();
    let mut flavored: Vec<String> = Vec::new();
    fn push(r: Requirement, out: &mut Vec<Requirement>) {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    for req in reqs {
        let module = match &req.dep {
            BuildRequire::Capability(c) if !flavors.is_empty() => python_module(c),
            _ => None,
        };
        let Some((name, bound)) = module else {
            push(req.clone(), &mut requirements);
            continue;
        };
        for flavor in flavors {
            let binary = format!("{flavor}-{name}");
            if !flavored.contains(&binary) {
                flavored.push(binary.clone());
            }
            let r = Requirement {
                dep: BuildRequire::Package(binary),
                bound: bound.clone().or_else(|| req.bound.clone()),
            };
            push(r, &mut requirements);
        }
    }
    // A name the spec also writes itself keeps what a package name gets.
    flavored.retain(|f| {
        !reqs
            .iter()
            .any(|r| matches!(&r.dep, BuildRequire::Package(n) if n == f))
    });
    PythonModules {
        requirements,
        flavored,
    }
}
