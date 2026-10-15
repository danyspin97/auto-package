# python-cuda-pathfinder notes

## Build requirements
- The upstream wheel is pure Python (Root-Is-Purelib true, tag py3-none-any), so the package is noarch and the same build applies to every architecture.
- The wheel requires Python 3.10 or newer (Requires-Python >=3.10).
- No Requires: the wheel declares no runtime dependencies and no extras.
- The import name is `cuda`, a namespace directory; the real module is `cuda/pathfinder`, and the package owns the namespace directory.
- The wheel's dist-info license file goes in %license.
Checked against 1.8.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The placeholder %check was replaced with an import smoke test of `cuda.pathfinder`, because the wheel ships no tests to run.
Checked against 1.8.3 on openSUSE_Tumbleweed, x86_64.
