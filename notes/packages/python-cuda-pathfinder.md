# python-cuda-pathfinder notes

## Build requirements
- Upstream is a prebuilt pure-Python wheel, so the package is noarch and the same build applies to every architecture.
- No Requires: the wheel declares no dependencies.
- Package layout: the `cuda` directory is a namespace package, and `cuda/pathfinder` is the real module. The package owns the namespace directory; the wheel's dist-info LICENSE goes in %license.
Checked against 1.8.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check is an import smoke test of `cuda.pathfinder`, not a test suite, because the wheel ships no tests.
Checked against 1.8.3 on openSUSE_Tumbleweed, x86_64.
