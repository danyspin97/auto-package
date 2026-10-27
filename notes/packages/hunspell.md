# hunspell notes

## Build requirements
- No new BuildRequires for 1.7.6: configure's readline and curses checks are covered by the existing ones.
Checked against 1.7.6 on openSUSE_Factory, x86_64.

## Workarounds
- The libhunspell shared library symlink must target the file libtool produces for version-info 2:2:2, which is the .so.0.2.2 release, not .so.0.0.1. Fixed in the spec.
- The Hungarian man5 page is packaged and listed in %files, since 1.7.6 ships it.
Checked against 1.7.6 on openSUSE_Factory, x86_64.

## Failure history
- Package step failed with "File not found" for README in %doc: the 1.7.6 tarball ships README.md, not README. Fixed by dropping README from %doc; README.md, AUTHORS and THANKS remain listed.
Checked against 1.7.6 on openSUSE_Factory, x86_64.
