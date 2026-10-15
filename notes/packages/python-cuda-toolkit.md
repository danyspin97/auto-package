# python-cuda-toolkit

## Build requirements
- The package is a metapackage. The upstream wheel for the build architecture holds only its dist-info directory, and every NVIDIA component is an optional extra. The metadata has no unconditional Requires-Dist, so there is no base Requires.
- The wheel is pure Python (py3-none-any, Root-Is-Purelib: true), so the built package is architecture-independent.
- The upstream metadata has no License field, so the License tag is set by hand. The license is LicenseRef-NVIDIA-Proprietary, because the NVIDIA license has no SPDX identifier.
- %files lists only the dist-info directory, since nothing else is installed.
Checked against 13.0.3.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check has no test suite. It only asserts that the installed distribution metadata reports the package version.
Checked against 13.0.3.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Spec comments must not contain the macro character, because it expands even inside comments. Reword them without it.
- The changelog generator needs a D-Bus session. Without one, write the changelog entry by hand.
Checked against 13.0.3.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The changelog generator failed for lack of a D-Bus session. Fixed by writing the changelog entry by hand.
Checked against 13.0.3.0 on openSUSE_Tumbleweed, x86_64.
