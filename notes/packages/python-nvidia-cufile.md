# python-nvidia-cufile

## Build requirements
- The package repackages the upstream prebuilt wheel, so there is no compile step. The spec installs the wheel contents, not a build from source.
- Root-Is-Purelib is true, so the wheel's Python files go under sitelib.
- ExclusiveArch is limited to the architectures the upstream wheel ships for. Do not mark the package noarch, because the wheel contains native shared objects.
- The wheel's dist-info license file is listed with %license.
Checked against 1.15.1.6 on openSUSE_Tumbleweed, x86_64.

## Test changes
None recorded.

## Workarounds
- The headers shipped in the wheel are left out of the package, and their RECORD entry is dropped from %files. This is a packaging choice, not a build fix.
Checked against 1.15.1.6 on openSUSE_Tumbleweed, x86_64.

## Failure history
- No build failures recorded. The build succeeded on the first attempt. Runtime loading of the packaged libraries was not checked.
Checked against 1.15.1.6 on openSUSE_Tumbleweed, x86_64.
