# crcpp

## Workarounds

- Source0 is the GitHub release archive. It unpacks to a directory named CRCpp-release-<version>, so the spec's %autosetup -n must use that name.
- The devel package owns the CPS share directory (%{_datadir}/cps/), which holds the CPS package description files.

Checked against 1.2.3.0 on openSUSE_Factory, x86_64.
