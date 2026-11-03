//! The rpmlintrc of a prebuilt wheel: whether a wheel is prebuilt and the filters
//! that its rpmlintrc holds.

/// Build a skeleton spec for a release that ships no sdist AND has no
/// discoverable source repository — a closed-source binary redistribution.
/// This is the normal shape of NVIDIA's individual CUDA toolkit component
/// wheels (`nvidia-cuda-nvcc`, `nvidia-cuda-cupti`, `nvidia-cuda-nvrtc`,
/// `nvidia-cuda-runtime`, ...): PyPI is simply how NVIDIA redistributes the
/// proprietary toolkit binaries, and there is nothing to compile.
///
/// `wheel_filename` becomes `Source0`; `%install` unpacks it directly with
/// `%pyproject_install`, which accepts a pre-built wheel path instead of a
/// dist it just built. See convention 24 Exception 3 for when this is the
/// right call versus a workaround to avoid.
/// Whether a repackaged wheel is the shape that reliably trips the same
/// handful of rpmlint false positives on an unmodified vendor binary: an
/// arch-specific wheel (not `-none-any.whl`) whose payload is a shared
/// library rather than importable Python. rpmlint judges these as if they
/// were a from-source build — a detached hash section, an executable bit on
/// the `.so`, `ldconfig` scriptlets, a runtime dependency on `python-base` —
/// none of which apply to a vendor `.so` dropped unmodified under
/// site-packages. See [`prebuilt_wheel_rpmlintrc`].
pub(crate) fn is_prebuilt_binary_wheel(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    let is_pure = wheel_filename.ends_with("-none-any.whl");
    !is_pure && layout.is_some_and(|l| l.first_shared_lib.is_some())
}

/// The `rpmlintrc` of a prebuilt/vendor-binary wheel (see
/// [`is_prebuilt_binary_wheel`]): only the findings that cannot be fixed in the
/// package, each with the reason it stays. What can be fixed is fixed in the
/// spec instead, by [`wheel_binary_skeleton`]: the executable bit on the
/// libraries, the dependency on `python-base`, the bundled headers.
///
/// - `missing-hash-section`: the libraries are upstream's prebuilt binaries,
///   linked without a build-id/hash section; they cannot be relinked here.
/// - `library-without-ldconfig-postin/postun`: the bundled libraries sit in
///   site-packages and are loaded by the Python modules from their own
///   directory, not from the ldconfig path, so there is no cache to refresh.
/// - `binary-or-shlib-defines-rpath` for `$ORIGIN`: vendor libraries find
///   their siblings that way, and the rule is badness 10000, so one hit aborts
///   rpmlint (`libcusparseLt.so.0 (RUNPATH: $ORIGIN:)`). Only written when a
///   bundled library really has such a RUNPATH
///   ([`ap_ecosystem::sdist::WheelLayout::origin_runpath`]): a filter nothing
///   matches is `unused-rpmlintrc-filter`, an error of its own (convention
///   72). Any other RPATH value still fails the check.
pub fn prebuilt_wheel_rpmlintrc(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::from(
        "# reason: the libraries are upstream's prebuilt binaries, linked without a hash section; they cannot be relinked here\n\
         addFilter(\"missing-hash-section\")\n\
         \n\
         # reason: the bundled libraries are loaded from their own directory by the Python modules, not from the ldconfig path, so there is no cache for a scriptlet to refresh\n\
         addFilter(\"library-without-ldconfig-postin\")\n\
         \n\
         # reason: the bundled libraries are loaded from their own directory by the Python modules, not from the ldconfig path, so there is no cache for a scriptlet to refresh\n\
         addFilter(\"library-without-ldconfig-postun\")\n",
    );
    if layout.origin_runpath {
        out.push_str(
            "\n\
             # reason: the bundled libraries find each other through an $ORIGIN RUNPATH, which is how a self-contained vendor wheel has to be laid out\n\
             addFilter(\".*binary-or-shlib-defines-rpath.*[$]ORIGIN\")\n",
        );
    }
    out
}

/// The `zero-length` filters for the empty files a wheel ships (see
/// [`ap_ecosystem::sdist::WheelLayout::empty_files`]), one per file with the
/// reason. The files are upstream's own placeholders (`custom.css`,
/// `loader.js`): making them non-empty would change the payload, so rpmlint is
/// told instead. A name with a character outside the plain set is left out
/// rather than written into a regex unescaped.
fn wheel_empty_files_filters(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::new();
    for name in &layout.empty_files {
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-@+~".contains(c))
        {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let escaped = name.replace('.', "\\.").replace('+', "\\+");
        out.push_str(&format!(
            "# reason: {name} is an empty placeholder that upstream's wheel ships on purpose (a user-editable stylesheet or script)\n\
             addFilter(r\".*zero-length .*/site-packages/{escaped}$\")\n"
        ));
    }
    out
}

/// Whether a repackaged wheel needs a `python-<name>.rpmlintrc`: it is a
/// vendor-binary wheel, or it ships empty files rpmlint calls `zero-length`.
pub(crate) fn wheel_needs_rpmlintrc(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    is_prebuilt_binary_wheel(wheel_filename, layout)
        || layout.is_some_and(|l| !wheel_empty_files_filters(l).is_empty())
}

/// Write `python-<name>.rpmlintrc` next to the spec for a wheel that needs one
/// (the spec's `Source99:` names it, see [`wheel_needs_rpmlintrc`]); nothing for
/// any other wheel.
pub(crate) fn wheel_rpmlintrc(
    pkg_name: &str,
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> Option<(String, String)> {
    if !wheel_needs_rpmlintrc(wheel_filename, layout) {
        return None;
    }
    let layout = layout?;
    let mut text = String::new();
    if is_prebuilt_binary_wheel(wheel_filename, Some(layout)) {
        text.push_str(&prebuilt_wheel_rpmlintrc(layout));
    }
    let empties = wheel_empty_files_filters(layout);
    if !empties.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&empties);
    }
    // A zero-byte rpmlintrc is a `Source99` that does nothing: never written.
    if text.trim().is_empty() {
        return None;
    }
    Some((
        format!("python-{}.rpmlintrc", pkg_name.to_lowercase()),
        text,
    ))
}
