use super::*;

/// The build-time print lines are exempt; a flag and a print elsewhere are not.
#[test]
fn a_build_print_is_exempt_from_the_bullet_rule() {
    assert!(is_build_print("%check", "cat test-suite.log"));
    assert!(!is_build_print("%check", "make check"));
    assert!(!is_build_print("%prep", "echo hi"));
}

/// Each line that only looks like a print, and does more, is not one.
#[test]
fn a_print_that_writes_deletes_or_sets_is_not_a_print() {
    for line in [
        "cat x > %{buildroot}/usr/share/foo",
        "echo 'x' >> %{buildroot}/etc/foo.conf",
        "find %{buildroot} -name '*.la' -delete",
        "find . -name '*.o' -exec rm {} ;",
        "ls test/x.c | grep -q foo || rm test/x.c",
        "env CFLAGS=-O0 make -C sub",
        "set -x; ./configure --prefix=/usr",
        "set -xe",
        "VAR=1 cat test-suite.log",
        "echo $(rm -f x)",
        "cat `which foo`",
        "echo hi && make install",
    ] {
        assert!(!is_build_print("%check", line), "{line}");
    }
}

/// The plain prints stay prints, and `env` and `set -x` alone are too.
#[test]
fn plain_prints_are_prints() {
    for line in [
        "echo ---",
        "cat test-suite.log",
        "ls -la",
        "set -x",
        "set +x",
        "env",
        "find . -name x",
    ] {
        assert!(is_build_print("%check", line), "{line}");
    }
}
