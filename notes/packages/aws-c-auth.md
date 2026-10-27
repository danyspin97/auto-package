# aws-c-auth notes

## Workarounds
- The spec's `library_version` macro must match the upstream release version being built. The SONAME symlink and the `%files` entries are derived from it, so every version bump has to update this macro as well as Source0.
Checked against 1.0.2 on openSUSE_Factory, x86_64.
