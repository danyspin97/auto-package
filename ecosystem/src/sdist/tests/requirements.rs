use super::*;

/// A `[build-system] requires` entry with a PEP 508 marker keeps its
/// requirement; a marker with a spec- or shell-special character drops it.
#[test]
fn build_requirement_part_keeps_the_requirement_and_refuses_unsafe_markers() {
    for (entry, want) in [
        ("numpy>=2; python_version>='3.9'", Some("numpy>=2")),
        ("tomli; python_version < \"3.11\"", Some("tomli")),
        (
            "pkg[extra] >= 1.0 ; python_version < '3' and sys_platform == \"linux\"",
            Some("pkg[extra] >= 1.0"),
        ),
        ("setuptools >= 42, < 80", Some("setuptools >= 42, < 80")),
        ("numpy>=2;", Some("numpy>=2")),
        ("tomli; python_version%{lua:x}", None),
        ("tomli; python_version\n%pre", None),
        ("tomli; python_version < '3'; x", None),
        ("tomli; $(x) == '3'", None),
        ("tomli; a`x` == 'b'", None),
        ("tomli; a|b", None),
        ("tomli; a&b", None),
        ("tomli; a\\b", None),
        ("tomli; a # b", None),
        ("tomli; a{b}", None),
        ("setuptools\n%pre; python_version < '3'", None),
        (
            "pkg @ https://example.org/x.tar.gz; python_version < '3'",
            None,
        ),
        ("pkg @ https://example.org/x.tar.gz", None),
    ] {
        assert_eq!(
            build_requirement_part(entry),
            want.map(|w| BuildRequirement::Keep(w.into())),
            "{entry:?}"
        );
    }
}

/// A marker false for the build target (Linux, CPython) drops the
/// requirement; one that is undecided for it, mixed, or unreadable keeps it.
#[test]
fn build_requirement_part_drops_what_the_build_target_excludes() {
    use BuildRequirement::{Keep, NotForTarget};
    for (entry, want) in [
        ("colorama; sys_platform == \"win32\"", NotForTarget),
        ("pywin32; os_name == 'nt'", NotForTarget),
        ("tomli; python_version < \"3.11\"", Keep("tomli".into())),
        ("foo; sys_platform == 'linux'", Keep("foo".into())),
        (
            "foo; sys_platform == 'win32' or sys_platform == 'darwin'",
            NotForTarget,
        ),
        (
            "foo; python_version < '3.8' and sys_platform == 'win32'",
            NotForTarget,
        ),
        (
            "foo; sys_platform == 'win32' or python_version < '3.8'",
            Keep("foo".into()),
        ),
        ("foo; 'win32' == sys_platform", NotForTarget),
        ("foo; sys_platform != 'linux'", NotForTarget),
        (
            "foo; platform_system == 'Linux' and implementation_name == 'pypy'",
            NotForTarget,
        ),
        (
            "foo; platform_python_implementation == 'PyPy'",
            NotForTarget,
        ),
        ("foo; sys_platform in 'win32 darwin'", NotForTarget),
        ("foo; sys_platform not in 'linux darwin'", NotForTarget),
        (
            "foo; sys_platform not in 'win32 darwin'",
            Keep("foo".into()),
        ),
        ("foo; 'win' in sys_platform", NotForTarget),
        ("foo; 'lin' in sys_platform", Keep("foo".into())),
        (
            "foo; sys_platform == 'win32' and platform_machine == 'x86_64'",
            NotForTarget,
        ),
        ("foo; platform_machine == 'arm64'", Keep("foo".into())),
        ("foo; extra == 'test'", Keep("foo".into())),
        ("foo; sys_platform < 'win32'", Keep("foo".into())),
        (
            "foo; sys_platform == 'win32' or sys_platform == 'linux' and python_version < '3'",
            Keep("foo".into()),
        ),
        ("foo; (sys_platform == 'win32')", NotForTarget),
        ("foo; not sys_platform == 'win32'", Keep("foo".into())),
        ("foo; sys_platform == 'win32", Keep("foo".into())),
        ("foo; sys_platform == 'win32' and", Keep("foo".into())),
        (
            "foo; sys_platform == 'win32' == 'linux'",
            Keep("foo".into()),
        ),
        (
            "foo; (sys_platform == 'win32' or sys_platform == 'darwin') and python_version >= '3.8'",
            NotForTarget,
        ),
        (
            "foo; (python_version < '3.8' or sys_platform == 'linux')",
            Keep("foo".into()),
        ),
        ("foo; not (sys_platform == 'win32')", Keep("foo".into())),
        ("foo; (sys_platform == 'win32'", Keep("foo".into())),
        ("foo; sys_platform == 'win32')", Keep("foo".into())),
        ("foo; ((sys_platform == 'win32'))", NotForTarget),
        (
            "foo; not (sys_platform == 'win32') and python_version < '3'",
            Keep("foo".into()),
        ),
        (
            "foo; not (sys_platform == 'win32') or sys_platform == 'win32'",
            Keep("foo".into()),
        ),
        (
            "foo; not (sys_platform == 'linux') or sys_platform == 'darwin'",
            NotForTarget,
        ),
        (
            "foo; (sys_platform == 'win32' or python_version < '3.8') and sys_platform == 'linux'",
            Keep("foo".into()),
        ),
        ("foo; ()", Keep("foo".into())),
        (
            "foo; sys_platform == 'win32' and python_version < '3' or sys_platform == 'darwin'",
            NotForTarget,
        ),
    ] {
        assert_eq!(build_requirement_part(entry), Some(want), "{entry:?}");
    }
}

/// A parenthesized version list is accepted and written without its
/// parentheses, in the `name op version` form a spec macro takes.
#[test]
fn build_requirement_part_writes_a_parenthesized_version_list_without_parens() {
    for (entry, want) in [
        ("setuptools (>=42)", "setuptools >= 42"),
        ("foo (>=1.0, <2)", "foo >= 1.0, < 2"),
        ("pkg[extra] (>=1.0)", "pkg[extra] >= 1.0"),
        ("foo(>=1)", "foo >= 1"),
        ("setuptools>=42", "setuptools>=42"),
    ] {
        let Some(BuildRequirement::Keep(part)) = build_requirement_part(entry) else {
            panic!("{entry:?} should be kept");
        };
        assert_eq!(part, want, "{entry:?}");
        assert!(!part.contains('('), "{entry:?}");
    }
    assert_eq!(build_requirement_part("foo (>=1; $(x))"), None);
}

/// Through `[build-system] requires`: the excluded requirement is not
/// kept, the others are, in order.
#[test]
fn build_system_drops_requirements_the_build_target_excludes() {
    let doc: PyprojectToml = toml::from_str(
            "[build-system]\n\
             requires = [\"setuptools\", \"colorama; sys_platform == 'win32'\", \"tomli; python_version < '3.11'\"]\n\
             build-backend = \"setuptools.build_meta\"\n",
        )
        .unwrap();
    let build = BuildSystem::from(doc.build_system.unwrap());
    assert_eq!(build.requires, vec!["setuptools", "tomli"]);
}

/// The marker is parsed only after the charset check: an unsafe marker is
/// refused even when the clause around it would be decided.
#[test]
fn unsafe_marker_is_refused_not_decided() {
    assert_eq!(
        build_requirement_part("foo; sys_platform == 'win32'%{x}"),
        None
    );
    assert_eq!(
        build_requirement_part("foo; sys_platform == 'win32'; x"),
        None
    );
}

/// Through the `[build-system]` parse: a marker is dropped, the
/// requirement kept, and a direct reference is refused.
#[test]
fn build_system_keeps_requirements_whose_marker_is_dropped() {
    let doc: PyprojectToml = toml::from_str(
            r#"[build-system]
requires = ["numpy>=2; python_version>='3.9'", 'tomli; python_version < "3.11"', "pkg @ https://example.org/x.tar.gz", "wheel"]
"#,
        )
        .unwrap();
    let bs = BuildSystem::from(doc.build_system.unwrap());
    assert_eq!(bs.requires, vec!["numpy>=2", "tomli", "wheel"]);
}

/// Python's tarfile writes directory members without a trailing slash; they
/// must be recognised as directories and kept out of the file listing.
#[test]
fn directory_entries_without_trailing_slash_are_not_files() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    {
        let file = std::fs::File::create(&tarball).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        let mut d = tar::Header::new_gnu();
        d.set_entry_type(tar::EntryType::Directory);
        d.set_size(0);
        d.set_mode(0o755);
        d.set_cksum();
        ar.append_data(&mut d, "thing-1.0/lib", std::io::empty())
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "thing-1.0/lib/x.py", &b"hi"[..])
            .unwrap();
        ar.into_inner().unwrap().finish().unwrap();
    }
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.files, vec!["thing-1.0/lib/x.py".to_string()]);
    assert_eq!(info.top_level_dir.as_deref(), Some("thing-1.0"));
}
