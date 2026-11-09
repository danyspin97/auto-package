//! Item 7 of the translate-deps prompt: a dependency that is missing only
//! through its PEP 508 extra is not a flavor gap and not a missing package.

/// Spliced into the translate-deps prompt before its closing "Exception"
/// paragraph. It is the value of a format argument, so its braces are literal.
pub(crate) const EXTRA_ITEM: &str = "7. **An extra is not a capability.** `nothing provides \
    python313-setuptools_scm[toml]` (any `python<NNN>-<name>[<extra>]`) where the plain \
    `python<NNN>-<name>` is published means the package exists and the spec asks for a PEP 508 \
    extra that no distro package provides. That is NOT a flavor gap (item 4) and NOT a missing \
    package: do not report it in `unresolvable_packages`. Remove the extra from the spec and keep \
    the version bound: `%{python_module setuptools-scm[toml] >= 6.2}` becomes \
    `%{python_module setuptools_scm >= 6.2}`, using the distro's spelling of the name.\n\n";

#[cfg(test)]
mod tests {
    use super::EXTRA_ITEM;

    #[test]
    fn the_extra_item_says_drop_the_extra_and_not_flavor_gap() {
        assert!(EXTRA_ITEM.starts_with("7. "), "{EXTRA_ITEM}");
        assert!(EXTRA_ITEM.contains("NOT a flavor gap"), "{EXTRA_ITEM}");
        assert!(EXTRA_ITEM.contains("NOT a missing"), "{EXTRA_ITEM}");
        assert!(EXTRA_ITEM.contains("Remove the extra"), "{EXTRA_ITEM}");
        assert!(
            EXTRA_ITEM.contains("%{python_module setuptools_scm >= 6.2}"),
            "{EXTRA_ITEM}"
        );
        assert!(EXTRA_ITEM.ends_with("\n\n"));
    }
}
