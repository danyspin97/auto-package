# python-nvidia-cudnn-frontend notes

## Build requirements
- The source build is not used. It needs the CUDA toolkit (CMake requires CUDAToolkit) and cuDNN headers and library at build time, and no usable CUDA or cuDNN development packages were available. The package ships the upstream wheel for the build architecture instead, one per CPython flavor (cp310 to cp314, plus free-threaded cp314t).
- Upstream wheels were only checked for x86_64 and aarch64. Any other architecture needs its own check before reusing this layout.
- The C++ headers (include/) go in a devel subpackage, because rpmlint flagged them in the main package.
- The main package needs a Requires that was missing before the rpmlint fix below.
Checked against 1.31.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Upstream wheel for the build architecture instead of a source build; reason as in Build requirements.
- The shebang is removed from the hstu Python module, because rpmlint flagged it on a non-executable module.
Checked against 1.31.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The %files list still had an unfilled TODO placeholder, so the spec was rejected. Fixed by listing the paths of the upstream wheel per flavor (cudnn/, include/, dist-info, licenses).
- rpmlint badness 9568 (threshold 999) after %install and %check passed. Fixed by moving the headers to the devel subpackage, correcting the summary, adding the main Requires and removing the hstu shebang. Passed in the Tumbleweed x86_64 build.
Checked against 1.31.0 on openSUSE_Tumbleweed, x86_64.
