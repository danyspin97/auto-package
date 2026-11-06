//! One package per name: a project that lists two spellings of one name
//! (`python-authlib` and `python-Authlib`) is one package to update, not two.

use crate::osc;
use std::collections::BTreeMap;

/// `names` with one spelling of each name kept, in the order given, and the
/// spellings dropped as `(dropped, kept)`. Names are the same when their
/// [`osc::fuzzy_key`] is. The spelling kept is the all-lowercase one, which is
/// how the pipeline names a package, else the smallest (the rule of
/// `preferred_own_spelling`, so the plan and the build agree).
pub(super) fn one_spelling_each(names: Vec<String>) -> (Vec<String>, Vec<(String, String)>) {
    let mut by_key: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for n in &names {
        by_key.entry(osc::fuzzy_key(n)).or_default().push(n);
    }
    let mut dropped = Vec::new();
    let mut keep: Vec<&String> = Vec::new();
    for group in by_key.values() {
        let kept = group
            .iter()
            .find(|n| **n == &n.to_lowercase())
            .or_else(|| group.iter().min())
            .unwrap_or(&group[0]);
        keep.push(kept);
        dropped.extend(
            group
                .iter()
                .filter(|n| *n != kept)
                .map(|n| ((*n).clone(), (*kept).clone())),
        );
    }
    let kept: Vec<String> = names.iter().filter(|n| keep.contains(n)).cloned().collect();
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn two_spellings_of_a_name_are_one_package() {
        let (kept, dropped) =
            one_spelling_each(names(&["python-Authlib", "python-authlib", "python-attrs"]));
        assert_eq!(kept, names(&["python-authlib", "python-attrs"]));
        assert_eq!(
            dropped,
            vec![("python-Authlib".to_string(), "python-authlib".to_string())]
        );
    }

    #[test]
    fn without_a_lowercase_spelling_the_smallest_is_kept() {
        let (kept, _) = one_spelling_each(names(&["Foo_Bar", "Foo-Bar"]));
        assert_eq!(kept, names(&["Foo-Bar"]));
    }

    #[test]
    fn distinct_names_are_all_kept() {
        let (kept, dropped) = one_spelling_each(names(&["a", "b"]));
        assert_eq!(kept, names(&["a", "b"]));
        assert!(dropped.is_empty());
    }
}
