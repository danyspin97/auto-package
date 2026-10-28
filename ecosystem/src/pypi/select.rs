use super::*;

/// The source tarball among the release's files.
///
/// A release usually has one sdist and many wheels, but the format is not
/// guaranteed: older uploads are `.zip`, and a few have both. Prefer
/// `.tar.gz`, which is what every openSUSE Python spec expects.
///
/// A yanked file is disqualifying for the release (see `parse_release`), so a
/// live sdist of any format beats a yanked `.tar.gz`. Among candidates with the
/// same yanked state the format preference still applies; if every sdist is
/// yanked, the result is the same as before this rule existed.
pub(super) fn pick_sdist(urls: &[RawFile]) -> Option<&RawFile> {
    let sdists = || urls.iter().filter(|f| f.packagetype == "sdist");
    let live = || sdists().filter(|f| !f.yanked);
    live()
        .find(|f| f.filename.ends_with(".tar.gz"))
        .or_else(|| live().next())
        .or_else(|| sdists().find(|f| f.filename.ends_with(".tar.gz")))
        .or_else(|| sdists().next())
}

/// Pick a wheel to repackage as a prebuilt binary, for a release that has
/// neither an sdist nor a discoverable source repository. Prefers a
/// universal `py3-none-any` wheel (installable regardless of platform), then
/// a wheel whose platform tag names `arch`, then whatever wheel exists.
///
/// `single_locked_flavor_abi` is the CPython ABI tag (`cp313`, not
/// `python313`) of the one Python flavor this project is configured to build
/// for, when it is configured for exactly one — see
/// `ap_build::flavors::PythonFlavors::single_flavor`, which is the only
/// producer of this value and documents why anything other than exactly one
/// configured flavor must pass `None` here. `None` otherwise (including
/// "don't know"), which keeps the historical, maximally conservative
/// behavior of refusing every CPython-ABI-locked wheel.
pub fn pick_wheel_from_json(
    json: &str,
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> Result<WheelFile, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = &raw.info;
    let wheel =
        pick_wheel(&raw.urls, arch, single_locked_flavor_abi).ok_or_else(|| Error::NoWheel {
            name: info.name.clone(),
            version: info.version.clone(),
        })?;
    let sha256 = wheel
        .digests
        .sha256
        .clone()
        .ok_or_else(|| Error::NoDigest(wheel.filename.clone()))?;
    Ok(WheelFile {
        filename: wheel.filename.clone(),
        url: wheel.url.clone(),
        sha256,
        size: wheel.size,
    })
}

/// One wheel per CPython flavor, for a release whose wheels are each locked to
/// a single CPython (`cp313-cp313`, `cp314-cp314` — Triton's shape), where no
/// single wheel can serve every flavor. `abis` are ABI tags (`cp313`).
///
/// Fails with [`Error::NoWheel`] if ANY flavor has no installable wheel for
/// `arch`: a package missing one flavor must not be silently half-built.
pub fn pick_wheels_per_flavor(
    json: &str,
    arch: &str,
    abis: &[String],
) -> Result<Vec<(String, WheelFile)>, Error> {
    let mut out: Vec<(String, WheelFile)> = Vec::new();
    for abi in abis {
        let wheel = pick_wheel_from_json(json, arch, Some(abi))?;
        out.push((abi.clone(), wheel));
    }
    Ok(out)
}

fn pick_wheel<'a>(
    urls: &'a [RawFile],
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> Option<&'a RawFile> {
    let installable = || {
        urls.iter().filter(|f| {
            f.packagetype == "bdist_wheel"
                && !f.yanked
                && wheel_is_installable(&f.filename, arch, single_locked_flavor_abi)
        })
    };
    // Prefer the most-portable `-none-any.whl` among the installable set,
    // then anything else that passes. Previously the fallback chain ended in
    // `wheels().next()` (any wheel of the right ABI, regardless of its
    // platform or Python tag), which the Oct 6 review reproduced as:
    //   - an aarch64 request accepting `py3-none-win_amd64`,
    //   - a Python 3.13-locked project accepting `py2-none-any`.
    // The platform/Python checks are inside `wheel_is_installable` now, so
    // nothing incompatible can land in `installable()` to fall back onto.
    installable()
        .find(|f| f.filename.ends_with("-none-any.whl"))
        .or_else(|| installable().next())
}

/// Full PEP 425 / PEP 600 tag-triple compatibility check: a wheel's Python
/// tag, ABI tag, AND platform tag must all be installable on the target.
///
/// Composed of three independent predicates — `wheel_is_flavor_agnostic`
/// (ABI), [`wheel_python_is_installable`] (Python tag), and
/// [`wheel_platform_matches_linux_arch`] (platform tag). The Oct 6 review
/// found this check previously existed only for the ABI tag, so a wheel like
/// `py3-none-win_amd64` passed every filter on an aarch64 Linux target and
/// got silently repackaged into the spec; now the whole triple is validated
/// up front and the pick fails loudly rather than producing a broken binary.
fn wheel_is_installable(
    filename: &str,
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> bool {
    let Some((python, abi, platform)) = split_wheel_tags(filename) else {
        return false;
    };
    wheel_is_flavor_agnostic(filename, single_locked_flavor_abi)
        && wheel_python_is_installable(python, abi, single_locked_flavor_abi)
        && wheel_platform_matches_linux_arch(platform, arch)
}

/// Split a wheel filename into its (Python tag, ABI tag, platform tag)
/// triple, as the last three `-`-separated segments before `.whl`. Returns
/// `None` for a non-wheel filename or one that doesn't have those segments.
fn split_wheel_tags(filename: &str) -> Option<(&str, &str, &str)> {
    let stem = filename.strip_suffix(".whl")?;
    let (rest, platform) = stem.rsplit_once('-')?;
    let (rest, abi) = rest.rsplit_once('-')?;
    let (_, python) = rest.rsplit_once('-')?;
    Some((python, abi, platform))
}

/// Does this wheel's platform tag describe a filesystem layout installable
/// on `arch`-architecture Linux?
///
/// `any` is universal. The three Linux platform families — the historical
/// `linux_<arch>`, the baseline-pegged `manylinux*_<arch>`, and the
/// musl-targeted `musllinux_*_<arch>` — all end in `_<arch>`. Everything
/// else (`win_amd64`, `macosx_*`, `android_*`, ...) is not an openSUSE
/// target. A compressed platform tag like
/// `manylinux1_x86_64.manylinux_2_17_x86_64` is PEP 425's way of listing
/// multiple compatible platforms in one wheel; the `.`-split handles that.
pub(crate) fn wheel_platform_matches_linux_arch(tag: &str, arch: &str) -> bool {
    if tag == "any" {
        return true;
    }
    let suffix = format!("_{arch}");
    tag.split('.').any(|p| {
        let is_linux =
            p.starts_with("linux_") || p.starts_with("manylinux") || p.starts_with("musllinux_");
        is_linux && p.ends_with(&suffix)
    })
}

/// Does this wheel's Python tag describe an interpreter the openSUSE spec
/// will build for?
///
/// Three admitted shapes:
///   - `py<N>` / `py<N>.py<M>` — pure Python language level. `py3`/`py3x`
///     admitted; `py2` refused (we never build Python 2 flavors).
///   - `cp<XYZ>` — specific CPython version. Only safe when either (a) the
///     project is locked to exactly this flavor
///     (`single_locked_flavor_abi`), which is the same gate
///     `wheel_is_flavor_agnostic` enforces for the ABI tag, or (b) the ABI
///     tag is `abi3` (CPython's forward-compatible stable ABI), in which
///     case the wheel installs under any CPython ≥ the tagged version.
///   - Compressed `.`-separated combinations of either admitted shape.
///
/// Everything else — `py2*`, PyPy's `pp*`, Jython's `jp*`, IronPython's
/// `ip*`, anything we don't recognise — is refused. The Oct 6 review probed
/// this with a `py2-none-any` wheel being accepted for a Python 3.13 build.
pub(crate) fn wheel_python_is_installable(
    tag: &str,
    abi: &str,
    single_locked_flavor_abi: Option<&str>,
) -> bool {
    tag.split('.').any(|p| {
        if let Some(ver) = p.strip_prefix("py") {
            // `py<N>` or `py<N><M>` — language-level tag. First digit must
            // be '3' or newer (never Python 2).
            ver.chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() && c >= '3')
        } else if let Some(ver) = p.strip_prefix("cp") {
            // CPython-locked. Accept iff the project is pinned to exactly
            // this flavor OR the wheel uses the stable ABI (`abi3`) and we
            // have *some* CPython flavor configured to install into. A
            // bare `cp313` wheel against a dual-flavor project with no
            // single lock stays refused — same reasoning as
            // `wheel_is_flavor_agnostic`.
            if Some(p) == single_locked_flavor_abi {
                return true;
            }
            if abi == "abi3" {
                // Must still have a plausible digit-shape Python version
                // (not `cpABC`) and be an openSUSE-era Python (>= 3).
                return ver.chars().next().is_some_and(|c| c == '3');
            }
            false
        } else {
            false
        }
    })
}

/// Whether a wheel's ABI tag makes it safe to repackage as a single file,
/// either because it is agnostic to which Python flavor(s) an openSUSE spec
/// builds for (this project builds `python313`+`python314` side by side,
/// sometimes more), or because the project builds for only one flavor and
/// this wheel is locked to exactly that one.
///
/// ABI tag `none` means no CPython C-API dependency at all — pure Python, or
/// (the shape every NVIDIA CUDA toolkit wheel this fallback was built for
/// uses) a native library loaded via `dlopen`/`ctypes` rather than compiled
/// as a Python C extension — so the same file installs correctly under any
/// flavor. `abi3` is similar: a stable CPython ABI that works across every
/// CPython 3.x. A `cpXYZ` ABI tag (`cp313`, `cp310t`, ...) means the
/// opposite: compiled against one specific CPython's C API. Picking one and
/// repackaging it as if it worked everywhere produces a package that is
/// broken for every flavor except the one it happened to be built for —
/// better to find no usable wheel at all than a silently wrong one (Triton
/// is the real example: it publishes wheels, but one per CPython version
/// with no universal one, so this fallback must refuse rather than pick an
/// arbitrary `cp3xx` build) — *unless* `single_locked_flavor_abi` says this
/// project only ever builds for the one flavor this exact wheel is locked
/// to, in which case there is no second flavor left for it to be broken
/// under.
pub(super) fn wheel_is_flavor_agnostic(
    filename: &str,
    single_locked_flavor_abi: Option<&str>,
) -> bool {
    let Some((_python, abi, _platform)) = split_wheel_tags(filename) else {
        return false;
    };
    abi == "none" || abi == "abi3" || Some(abi) == single_locked_flavor_abi
}
