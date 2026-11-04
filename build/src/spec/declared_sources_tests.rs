use super::{conditional_source_files, declared_source_files};

const SPEC: &str = "\
Name:           foo
Version:        1.2
Release:        0
Summary:        x
License:        MIT
URL:            https://example.org/foo
%define mirror https://example.org/dl
Source0:        %{mirror}/%{name}-%{version}.tar.gz
Source1:        foo-%{version}.tar.gz.asc
Source2:        %{name}.keyring
Source3:        https://example.org/raw/blob#/renamed.bin
Source4:        %{unknown}/thing.tar.xz
Source99:       %{name}-rpmlintrc
Patch0:         not-a-source.patch
";

#[test]
fn every_source_file_is_named_after_the_expansion() {
    let (files, _) = declared_source_files(SPEC);
    assert_eq!(
        files,
        [
            "foo-1.2.tar.gz",
            "foo-1.2.tar.gz.asc",
            "foo.keyring",
            "renamed.bin",
            "foo-rpmlintrc"
        ]
    );
}

#[test]
fn a_source_with_a_macro_that_cannot_be_expanded_is_listed_apart() {
    let (_, unexpanded) = declared_source_files(SPEC);
    assert_eq!(unexpanded, ["%{unknown}/thing.tar.xz"]);
}

#[test]
fn a_spec_without_a_header_declares_nothing() {
    assert_eq!(declared_source_files("nothing"), (vec![], vec![]));
}

#[test]
fn a_source_inside_a_conditional_is_named_apart() {
    let spec = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: s\nLicense: MIT\n\
                Source0: foo-%{version}.tar.gz\n\
                %if 0%{?suse_version} > 1600\nSource1: extra-%{version}.tar.gz\n%if 1\nSource2: deep.tar.gz\n%endif\n%endif\n\
                Source3: after.tar.gz\n";
    assert_eq!(
        conditional_source_files(spec),
        ["extra-2.0.tar.gz", "deep.tar.gz"]
    );
    assert!(conditional_source_files("nothing").is_empty());
}
