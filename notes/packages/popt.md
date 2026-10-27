# popt

## Build requirements
- configure.ac calls LT_INIT and the external AM_GNU_GETTEXT and has no external library checks. The build needs libtool and the gettext macros; no library BuildRequires is needed for configure.
Checked against 1.19-release on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Upstream versions containing a hyphen (for example 1.19-release) cannot go in the Version tag, because rpm rejects the character. Set Version to the part before the hyphen and keep the full name in the Source0 filename.
- The %autosetup -n directory must match the top-level directory the tarball extracts to, including the suffix that Version drops. Building it from the version macro alone gives the wrong directory.
- Do not ship static libraries. rpmlint flags them when built with LTO (lto-no-text-in-archive) and when they lack debuginfo.
- A main package that holds only documentation is flagged no-binary. Do not keep one; ship the library and devel subpackages.
Checked against 1.19-release on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmspec rejected the hyphen in the Version tag (illegal character) -> split the version and keep the suffix in Source0, as above.
- Installed but unpackaged /usr/share/man/man3/popt.3.gz -> listed the man3 page in the devel subpackage.
- rpmlint Badness 10002 from the static library and the doc-only main package -> removed the static library and the main package; this build passed.
Checked against 1.19-release on openSUSE_Tumbleweed, x86_64.
