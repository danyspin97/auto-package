//! Whether a spec's `ExclusiveArch:` and `ExcludeArch:` keep the package from
//! being built on an architecture: such a build ends `excluded` on OBS.

/// The architectures `spec`'s top-level `ExclusiveArch:` allows when none of
/// them is `arch`, so a build for `arch` can only end `excluded`. `None` when the
/// spec has no such line, names a macro or `do_not_build` (a switch the tool
/// owns), has it inside a conditional, or allows `arch`.
pub fn exclusive_arch_excluding(spec: &str, arch: &str) -> Option<Vec<String>> {
    let allowed = top_level_arches(spec, "ExclusiveArch")?;
    if allowed.is_empty()
        || allowed
            .iter()
            .any(|a| a.contains('%') || a == "do_not_build")
    {
        return None;
    }
    (!lists_arch(&allowed, arch)).then_some(allowed)
}

/// Why a build for `arch` cannot succeed by the spec's own arch tags: the
/// `ExclusiveArch:` that excludes it, or an `ExcludeArch:` that names it. `None`
/// when the spec allows the build, or says nothing the tool can read (a macro,
/// a conditional): the build then decides.
pub fn arch_excluded_reason(spec: &str, arch: &str) -> Option<String> {
    if let Some(allowed) = exclusive_arch_excluding(spec, arch) {
        return Some(format!(
            "ExclusiveArch excludes {arch} (the spec allows {})",
            allowed.join(" ")
        ));
    }
    let excluded = top_level_arches(spec, "ExcludeArch")?;
    (!excluded.iter().any(|a| a.contains('%')) && lists_arch(&excluded, arch))
        .then(|| format!("ExcludeArch names {arch}"))
}

/// Whether the architecture list `list` names `arch`. A 32-bit x86 name stands
/// for every 32-bit x86 one (`i586` in the list covers an `i686` build).
fn lists_arch(list: &[String], arch: &str) -> bool {
    let is_x86_32 = |a: &str| matches!(a, "i386" | "i486" | "i586" | "i686");
    list.iter()
        .any(|a| a == arch || (is_x86_32(arch) && is_x86_32(a)))
}

/// The words of the top-level `tag:` lines of `spec`, or `None` when the tag
/// sits in a conditional, where its value is not known here. The lines of a
/// subpackage (after `%package`) do not count.
fn top_level_arches(spec: &str, tag: &str) -> Option<Vec<String>> {
    let prefix = format!("{tag}:");
    let mut depth = 0usize;
    let mut values: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        if t.starts_with("%description") || t.starts_with("%package") {
            break;
        }
        if t.starts_with("%if") {
            depth += 1;
        } else if t.starts_with("%endif") {
            depth = depth.saturating_sub(1);
        } else if t
            .get(..prefix.len())
            .is_some_and(|h| h.eq_ignore_ascii_case(&prefix))
        {
            if depth > 0 {
                return None;
            }
            values.extend(t[prefix.len()..].split_whitespace().map(str::to_string));
        }
    }
    Some(values)
}
