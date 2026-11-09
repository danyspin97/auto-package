//! The `50-check-packaged-twice` block, on the tail of python-kivy's build.log
//! (run 20261109-173516, build 14): rpmbuild and the tests had passed, and the
//! post-build check named two subpackages that install the same headers.

use super::*;
use crate::log::phase::segment;
use crate::log::{PathMapper, extract, strip_timestamp};

/// The tests' end, the logging noise pytest-xvfb prints at exit, then the
/// post-build checks, timestamps kept.
const KIVY_TAIL: &str = r#"[  465s] = 813 passed, 27 skipped, 84 deselected, 1 xfailed, 7 warnings, 52 subtests passed in 45.20s =
[  465s] [DEBUG  ] unset $DISPLAY
[  465s] --- Logging error ---
[  465s] Traceback (most recent call last):
[  465s]   File "/usr/lib64/python3.13/logging/__init__.py", line 1154, in emit
[  465s]     stream.write(msg + self.terminator)
[  465s]     ~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^
[  465s] ValueError: I/O operation on closed file.
[  465s] Call stack:
[  465s]   File "/usr/lib/python3.13/site-packages/pytest_xvfb.py", line 17, in shutdown_xvfb
[  465s]     xvfb_instance.stop()
[  488s] + exit 0
[  488s] ... checking for files with abuild user/group
[  489s] ... running 00-check-install-rpms
[  489s] ... installing all built rpms
[  489s] Verifying packages...
[  489s] python-Kivy-doc-2.3.2-90.1.noarch
[  490s] ... running 50-check-binary-kernel-log
[  490s] ... running 50-check-core-files
[  490s] ... checking for core files
[  490s] ... running 50-check-debuginfo
[  490s] ... testing for empty debuginfo packages
[  490s] ... running 50-check-filelist
[  490s] ... checking filelist
[  490s] ... running 50-check-gconf-scriptlets
[  490s] ... testing GConf scriptlet presence
[  490s] ... running 50-check-installtest
[  490s] ... testing for pre/postinstall scripts that are not idempotent
[  490s] ... running 50-check-invalid-provides
[  490s] ... running 50-check-invalid-requires
[  490s] dependency check for python-Kivy-debuginfo-2.3.2-90.1.x86_64.rpm:
[  490s] python-Kivy-debuginfo-2.3.2-90.1.x86_64
[  490s] ... running 50-check-kernel-build-id
[  490s] ... running 50-check-lanana
[  491s] ... running 50-check-libtool-deps
[  491s] ... testing devel dependencies required by libtool .la files
[  491s]     (can be skipped by "skip-check-libtool-deps" anywhere in spec)
[  491s] ... running 50-check-packaged-twice
[  491s] found conflict of python313-Kivy-devel-2.3.2-90.1.x86_64 with python314-Kivy-devel-2.3.2-90.1.x86_64:
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/graphics/cgl.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/common_subset.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/config.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/gl2platform.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/gl_redirect.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/khrplatform.h
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/tools/gles_compat/gl2.h
[  491s]
[  491s] ### VM INTERACTION START ###
[  491s] Powering off.
[  491s] ### VM INTERACTION END ###
[  491s] i04-ch2b failed "build python-Kivy.spec" at Mon Nov  9 18:18:34 UTC 2026.
"#;

fn located() -> Failure {
    let lines: Vec<&str> = KIVY_TAIL.lines().map(strip_timestamp).collect();
    let spans = segment(&lines);
    locate(&lines, &spans, &PathMapper::with_sdist_root("Kivy-2.3.2")).expect("a failure")
}

#[test]
fn the_packaged_twice_block_is_a_post_build_check() {
    let f = located();
    assert_eq!(f.genre, Genre::PostBuildCheck);
    assert_eq!(f.phase, Phase::PostBuild);
    assert_eq!(f.located_by, "packaged-twice");
}

#[test]
fn the_excerpt_names_the_check_and_every_path_of_the_conflict() {
    let f = located();
    assert!(f.excerpt.starts_with("... running 50-check-packaged-twice"));
    assert!(f.excerpt.contains(
        "found conflict of python313-Kivy-devel-2.3.2-90.1.x86_64 with python314-Kivy-devel-2.3.2-90.1.x86_64:"
    ));
    assert!(f.excerpt.contains("kivy/include/config.h"));
    assert!(f.excerpt.contains("tools/gles_compat/gl2.h"));
    assert_eq!(f.excerpt.matches("  - /usr/lib64/").count(), 7);
    // The block ends at the first line that is not indented under it.
    assert!(!f.excerpt.contains("Powering off"));
}

#[test]
fn the_check_block_is_found_through_the_whole_extract() {
    let e = extract(KIVY_TAIL, &PathMapper::with_sdist_root("Kivy-2.3.2"));
    assert!(e.has_signal());
    assert!(e.excerpt.contains("found conflict of"));
    assert_eq!(e.failed_section, None);
}
