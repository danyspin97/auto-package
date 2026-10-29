//! The license and description of a spec: the license of a wheel and of its
//! sdist, the SPDX expressions, the NVIDIA and GCC runtime terms, and the
//! description and changelog header checks.

use super::*;

/// `text` for a spec tag or description: one line, every `%` doubled so rpm does
/// not expand a macro out of upstream's wording.
pub(super) fn rpm_text(text: &str) -> String {
    text.replace('%', "%%")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `%description` body of a wheel spec: the METADATA's first paragraph,
/// wrapped, with every `%` doubled (rpm expands macros in a description), or the
/// summary when the wheel has none.
pub(super) fn wheel_description(paragraph: Option<&str>, summary: &str) -> String {
    let text = paragraph
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(summary);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.replace('%', "%%").split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 76 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines.join("\n")
}

/// Whether `file` (a path inside a wheel) is a licence text: a licence-named
/// leaf anywhere, or anything under `<dist-info>/licenses/`.
fn is_license_path(file: &str) -> bool {
    let leaf = file.rsplit('/').next().unwrap_or(file).to_ascii_uppercase();
    file.contains(".dist-info/licenses/")
        || ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| leaf.starts_with(p))
}

/// Whether the wheel ships a licence text of its own.
pub fn wheel_has_license_file(layout: &ap_ecosystem::sdist::WheelLayout) -> bool {
    layout.files.iter().any(|f| is_license_path(f))
}

/// The leaf name and bytes of the licence file at the top of a `.tar.gz` sdist
/// (`LICENSE`, `LICENSE.txt`, `COPYING`; the shortest name wins), at most 1 MiB.
pub fn sdist_top_level_license(tarball: &std::path::Path) -> Option<(String, Vec<u8>)> {
    use std::io::Read;
    let file = std::fs::File::open(tarball).ok()?;
    let mut archive = ap_ecosystem::sdist::bounded_tar_gz(file);
    let mut best: Option<(String, Vec<u8>)> = None;
    for entry in archive
        .entries()
        .ok()?
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
    {
        let Ok(entry) = entry else { continue };
        if !entry.header().entry_type().is_file() || entry.size() > (1 << 20) {
            continue;
        }
        let Ok(path) = entry.path() else { continue };
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let [_top, leaf] = parts.as_slice() else {
            continue;
        };
        let upper = leaf.to_ascii_uppercase();
        let plain = leaf
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        let named = ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| upper.starts_with(p));
        if !plain || !named || upper.ends_with(".PY") {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _)| b.len() <= leaf.len()) {
            continue;
        }
        let leaf = leaf.clone();
        let mut bytes = Vec::new();
        if entry.take(1 << 20).read_to_end(&mut bytes).is_ok() {
            best = Some((leaf, bytes));
        }
    }
    best
}

/// A wheel with no licence text of its own, whose sdist has one: copy it from
/// the sdist in `dir` (the checkout) next to the wheel and return its file name,
/// for [`with_license_source`]. `None` when the wheel has one, there is no sdist
/// there, or the sdist has none.
pub fn copy_sdist_license(
    dir: &std::path::Path,
    version: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
) -> Option<String> {
    if wheel_has_license_file(layout) {
        return None;
    }
    let mut tarballs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.ends_with(".tar.gz") && n.contains(version))
        })
        .collect();
    tarballs.sort();
    let (name, bytes) = tarballs.iter().find_map(|t| sdist_top_level_license(t))?;
    std::fs::write(dir.join(&name), bytes).ok()?;
    Some(name)
}

/// `spec` of a prebuilt wheel with the licence `file` (copied from the sdist by
/// [`copy_sdist_license`]) as `Source1`, copied to the build directory in
/// `%prep` and owned as `%license`, for a wheel that carries no licence text.
/// Left alone when the spec has a `%license` or a `Source1` already.
pub fn with_license_source(spec: &str, file: &str) -> String {
    if spec.contains("%license") || spec.contains("Source1:") {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        let t = line.trim_end();
        if t.starts_with("Source0:") {
            out.push_str(&format!(
                "# the wheel has no licence text; this one is the sdist's\nSource1:        {file}\n"
            ));
        } else if t == "%prep" {
            out.push_str(&format!("cp -p %{{SOURCE1}} {file}\n"));
        } else if t.starts_with("%files") && t.contains("%{python_files}") {
            out.push_str(&format!("%license {file}\n"));
        }
    }
    out
}

/// `spec` with a note under `%prep` that the release has no sdist and no source
/// repository, so the unpacked wheel is all there is. Only for a true no-sdist
/// release; a package switched to its wheel after a failed source build has a
/// sdist and gets no such note.
pub fn with_no_sdist_note(spec: &str) -> String {
    const NOTE: &str = "# this release has no sdist and no source repository: the upstream wheel is\n\
                        # installed as it is, so check the architecture and the file list against it\n";
    let mut out = String::with_capacity(spec.len() + NOTE.len());
    let mut done = false;
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.trim_end() == "%prep" {
            out.push_str(NOTE);
            done = true;
        }
    }
    out
}

/// Whether a `Requires-Python` specifier excludes a flavor the project builds
/// through an upper bound (`<3.13`, `<=3.12`, `==3.12.*`): pip then refuses to
/// install the wheel for that flavor unless told otherwise.
pub fn requires_python_caps_below(requires_python: &str, flavors: &[String]) -> bool {
    let versions: Vec<(u32, u32)> = flavors.iter().filter_map(|f| flavor_version(f)).collect();
    let parse = |v: &str| -> Option<(u32, u32)> {
        let mut it = v.trim().trim_end_matches(".*").split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().map_or(Some(0), |m| m.parse().ok())?;
        Some((major, minor))
    };
    requires_python.split(',').map(str::trim).any(|clause| {
        if let Some(v) = clause.strip_prefix("<=") {
            // `<=3.12` still allows 3.12.x; `<=3.12.5` allows all of 3.12.
            parse(v).is_some_and(|cap| versions.iter().any(|f| *f > cap))
        } else if let Some(v) = clause.strip_prefix('<') {
            let patch = v.trim().split('.').count() > 2;
            parse(v).is_some_and(|cap| {
                versions
                    .iter()
                    .any(|f| if patch { *f > cap } else { *f >= cap })
            })
        } else if let Some(v) = clause.strip_prefix("==") {
            v.trim().ends_with(".*")
                && parse(v).is_some_and(|only| versions.iter().any(|f| *f != only))
        } else {
            false
        }
    })
}

/// `spec` with `export PIP_IGNORE_REQUIRES_PYTHON=1` as the first line of `%build`
/// and `%install` (each is its own shell and each may run pip), when the wheel's
/// `Requires-Python` has an upper bound below a flavor that is built. A spec that
/// already sets the variable is returned as it is.
pub fn with_pip_ignore_requires_python(
    spec: &str,
    requires_python: Option<&str>,
    flavors: &[String],
) -> String {
    if spec.contains("PIP_IGNORE_REQUIRES_PYTHON")
        || !requires_python.is_some_and(|r| requires_python_caps_below(r, flavors))
    {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if matches!(line.trim_end(), "%build" | "%install") {
            out.push_str(
                "# the wheel caps Requires-Python below a Python we build for\n\
                 export PIP_IGNORE_REQUIRES_PYTHON=1\n",
            );
        }
    }
    out
}

/// SPDX id for an OSI classifier name, if it is one we know for certain.
fn classifier_spdx(name: &str) -> Option<&'static str> {
    Some(match name {
        "MIT License" => "MIT",
        "Apache Software License" => "Apache-2.0",
        "BSD License" => return None, // 2- vs 3-clause is not recoverable
        "ISC License (ISCL)" => "ISC",
        "Mozilla Public License 2.0 (MPL 2.0)" => "MPL-2.0",
        "GNU General Public License v2 (GPLv2)" => "GPL-2.0-only",
        "GNU General Public License v2 or later (GPLv2+)" => "GPL-2.0-or-later",
        "GNU General Public License v3 (GPLv3)" => "GPL-3.0-only",
        "GNU General Public License v3 or later (GPLv3+)" => "GPL-3.0-or-later",
        "GNU Lesser General Public License v3 (LGPLv3)" => "LGPL-3.0-only",
        "GNU Lesser General Public License v3 or later (LGPLv3+)" => "LGPL-3.0-or-later",
        "GNU Affero General Public License v3" => "AGPL-3.0-only",
        "Python Software Foundation License" => "PSF-2.0",
        "The Unlicense (Unlicense)" => "Unlicense",
        _ => return None,
    })
}

/// The `License:` value a wheel's own METADATA supports, or `None`: the SPDX
/// `License-Expression`, else the `License ::` classifiers that map to one SPDX
/// id, else a free-text `License:` that is a known SPDX id or spelling of one
/// (`MIT`, `Apache 2.0`, `3-clause BSD`) or NVIDIA's proprietary redistribution
/// text. Anything else stays `FIXME` for a person: a guessed licence is worse
/// than a flagged one.
pub(super) fn wheel_license(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
    if let Some(expr) = &layout.license_expression {
        return Some(spdx_expression_for_spec(expr));
    }
    if !layout.license_classifiers.is_empty() {
        let tag = license_tag(&License::Classifiers(layout.license_classifiers.clone()));
        if tag != "FIXME" {
            return Some(tag);
        }
    }
    spdx_from_free_text(layout.license_text.as_deref()?)
}

/// An SPDX expression for `License:`: a vendor's own licence reference
/// (`LicenseRef-NVIDIA-Proprietary`) is not a SPDX id and becomes the non-free
/// tag; anything else is kept as declared.
pub(super) fn spdx_expression_for_spec(expr: &str) -> String {
    // Only the term that names the vendor licence is replaced: the rest of an
    // expression (`MIT AND LicenseRef-NVIDIA-Proprietary`) stays.
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let low = word.to_ascii_lowercase();
        if low.contains("nvidia") || low.contains("proprietary") {
            out.push_str(NVIDIA_PROPRIETARY);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in expr.chars() {
        if c.is_whitespace() || c == '(' || c == ')' {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    // Two vendor terms are one. Dropping the second would leave its operator
    // dangling (`MIT AND SUSE-NonFree AND`), so the repeat is merged with the
    // operator that joins it instead.
    for op in ["AND", "OR"] {
        let twice = format!("{NVIDIA_PROPRIETARY} {op} {NVIDIA_PROPRIETARY}");
        while out.contains(&twice) {
            out = out.replace(&twice, NVIDIA_PROPRIETARY);
        }
    }
    out
}

/// The SPDX id a free-text licence name spells (`MIT`, `Apache 2.0`,
/// `3-clause BSD`), or the non-free tag for NVIDIA's proprietary redistribution
/// text; `None` for anything else.
fn spdx_from_free_text(text: &str) -> Option<String> {
    let key: String = text
        .to_ascii_lowercase()
        .replace(['_', ',', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let spdx = match key.as_str() {
        "mit" | "mit license" => "MIT",
        "mit-0" => "MIT-0",
        "apache 2 0"
        | "apache-2 0"
        | "apache license 2 0"
        | "apache license version 2 0"
        | "apache software license"
        | "apache software license 2 0"
        | "apache 2" => "Apache-2.0",
        "bsd-3-clause" | "bsd 3-clause" | "bsd 3 clause" | "3-clause bsd" | "3 clause bsd"
        | "new bsd" | "modified bsd" | "bsd-3" => "BSD-3-Clause",
        "bsd-2-clause" | "bsd 2-clause" | "bsd 2 clause" | "2-clause bsd" | "simplified bsd"
        | "freebsd" => "BSD-2-Clause",
        "isc" | "isc license" => "ISC",
        "mpl-2 0" | "mpl 2 0" | "mozilla public license 2 0" => "MPL-2.0",
        "psf" | "psf-2 0" | "python software foundation license" => "PSF-2.0",
        "unlicense" | "the unlicense" => "Unlicense",
        "zlib" => "Zlib",
        "0bsd" => "0BSD",
        _ => {
            let nvidia = key.contains("nvidia")
                && ["proprietary", "license agreement", "software license"]
                    .iter()
                    .any(|w| key.contains(w));
            return nvidia.then(|| NVIDIA_PROPRIETARY.to_string());
        }
    };
    Some(spdx.to_string())
}

/// The `License:` of a repackaged wheel: the release's own licence when it is a
/// usable tag, else what the wheel's METADATA supports (see [`wheel_license`]).
/// A free-text licence that is a known spelling is mapped to its SPDX id; one
/// that is prose or pasted licence text (multi-line, long) is never written into
/// the tag. A vendor's proprietary licence is the non-free tag.
pub(super) fn wheel_declared_license(
    release: &Release,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    let own = match &release.license {
        License::FreeText(text) => {
            let text = text.trim();
            match spdx_from_free_text(text) {
                Some(id) => id,
                None if text.is_empty() || text.contains('\n') || text.len() > 100 => {
                    "FIXME".to_string()
                }
                None => text.to_string(),
            }
        }
        License::Spdx(expr) => spdx_expression_for_spec(expr),
        other => license_tag(other),
    };
    match own.as_str() {
        "FIXME" => layout.and_then(wheel_license).unwrap_or(own),
        _ => own,
    }
}

/// The licence of a GCC runtime library bundled in a wheel (torch's `libgomp`).
const GCC_RUNTIME_LICENSE: &str = "GPL-3.0-or-later WITH GCC-exception-3.1";

/// Licence terms a wheel's own `License:` does not carry because the wheel
/// bundles a third party's binaries: NVIDIA's toolchain programs and the CUPTI
/// libraries (triton declares MIT but ships `ptxas`) are non-free, and GCC's
/// `libgomp` is GPL with the runtime exception. In a fixed order, no
/// duplicates; found from the file names only.
pub(super) fn wheel_vendor_license_terms(
    layout: &ap_ecosystem::sdist::WheelLayout,
) -> Vec<&'static str> {
    let mut nonfree = false;
    let mut gcc = false;
    for file in &layout.files {
        let top = file.split('/').next().unwrap_or("");
        if top.ends_with(".dist-info") || top.ends_with(".data") {
            continue;
        }
        let name = file.rsplit('/').next().unwrap_or(file);
        if matches!(
            name,
            "ptxas"
                | "ptxas-blackwell"
                | "cuobjdump"
                | "nvdisasm"
                | "nvlink"
                | "fatbinary"
                | "nvprune"
        ) || (name.starts_with("libcupti") && name.contains(".so"))
        {
            nonfree = true;
        }
        if name.starts_with("libgomp") && name.contains(".so") {
            gcc = true;
        }
    }
    let mut terms = Vec::new();
    if nonfree {
        terms.push(NVIDIA_PROPRIETARY);
    }
    if gcc {
        terms.push(GCC_RUNTIME_LICENSE);
    }
    terms
}

/// `license` with each of `terms` it does not already name, joined by `AND`
/// (an `OR` expression is parenthesised first). A licence still `FIXME` is left
/// for a person: terms beside an unknown are noise.
pub(super) fn with_license_terms(license: String, terms: &[&str]) -> String {
    if license == "FIXME" {
        return license;
    }
    let mut out = license;
    for term in terms {
        if out.contains(term) {
            continue;
        }
        // `AND` binds tighter than `OR`: an expression with an `OR` outside any
        // parentheses has to be grouped before a term is added to all of it.
        if has_top_level_or(&out) {
            out = format!("({out})");
        }
        out = format!("{out} AND {term}");
    }
    out
}

/// Whether an SPDX expression has an `OR` outside every pair of parentheses
/// (`MIT OR Apache-2.0`, `(A OR B) AND C OR D`; not `(A OR B)`).
pub(super) fn has_top_level_or(expr: &str) -> bool {
    let mut depth = 0i32;
    let mut word = String::new();
    let mut found = false;
    for c in expr.chars().chain(std::iter::once(' ')) {
        match c {
            '(' | ')' => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
                depth += if c == '(' { 1 } else { -1 };
            }
            c if c.is_whitespace() => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
            }
            c => word.push(c),
        }
    }
    found
}

pub(super) fn license_tag(license: &License) -> String {
    match license {
        License::Spdx(expr) => expr.clone(),
        License::FreeText(text) => text.clone(),
        License::Classifiers(classifiers) => {
            // Only classifiers with a known SPDX id are emitted: OSI display
            // names ("MIT License") are not valid in `License:`. When nothing
            // maps, `FIXME` is what the spec validator and agent already flag,
            // rather than an empty tag that looks like a finished spec.
            let ids: Vec<&str> = classifiers
                .iter()
                .filter_map(|c| c.strip_prefix("License :: OSI Approved :: "))
                .filter_map(classifier_spdx)
                .collect();
            if ids.is_empty() {
                "FIXME".to_string()
            } else {
                ids.join(" AND ")
            }
        }
        License::Unknown => "FIXME".to_string(),
    }
}

/// Whether a wheel's declared licence is a vendor's proprietary one rather than
/// an open source licence: free text or an SPDX expression naming NVIDIA or a
/// proprietary / end-user agreement, or the "Proprietary" trove classifier.
pub fn license_is_proprietary(license: &License) -> bool {
    let text = match license {
        License::Spdx(t) | License::FreeText(t) => t.to_lowercase(),
        License::Classifiers(c) => c.join(" ").to_lowercase(),
        License::Unknown => return false,
    };
    [
        "proprietary",
        "nvidia",
        "license agreement",
        "eula",
        "nonfree",
    ]
    .iter()
    .any(|w| text.contains(w))
}

/// `spec` with a sentence added to the `%description`, saying the package is a
/// vendor's prebuilt library unpacked from its wheel and not built from source.
pub fn note_prebuilt_library(spec: &str, proprietary: bool) -> String {
    let note = if proprietary {
        "This package redistributes the vendor's prebuilt binary library, unpacked from \
         its wheel; it is not built from source and its licence is not an open source one."
    } else {
        "This package redistributes the upstream prebuilt binary library, unpacked from \
         its wheel; it is not built from source."
    };
    let lines: Vec<&str> = spec.lines().collect();
    let insert_after = description_paragraph_end(&lines);
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        out.push(line.to_string());
        if Some(idx) == insert_after {
            out.push(String::new());
            out.push(note.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}
mod description;
pub use description::*;
