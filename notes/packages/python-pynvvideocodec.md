# python-pynvvideocodec notes

## Build requirements
- The upstream wheel for the build architecture is the only source; there is no sdist, so each Python flavor installs from the same wheel.
- The wheel is not pure-Python (Root-Is-Purelib is false), so its files belong in the platform library directory.
- The wheel metadata declares no runtime Requires-Dist, so runtime requirements come only from what the extension generates.
- The runtime requirement on libcuda.so.1 that the extension generates is excluded, because the NVIDIA driver provides it at run time and no package does.
Checked against 2.0.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check asserts the installed payload instead of importing the module, because importing calls the NVIDIA driver at import time and fails without it.
Checked against 2.0.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The wheel's data directory installs an FFmpeg source tree under the external directory of the prefix. That directory has no owner from pip, so it gets its own %dir entry.
- The FFmpeg files are shared by every Python flavor in one buildroot, and each file may be packaged once, so the FFmpeg entries are gated to the python314 flavor.
- Top-level samples/ and benchmarks/ directories that the wheel places in site-packages are removed.
- The bundled libav* and libxcb sonames are excluded from Provides, so they do not shadow system libraries for other packages.
- Do not edit the wheel's dist-info RECORD during install; rpm never reads it.
Checked against 2.0.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpm's file-list check rejected the FFmpeg data path because no package owned the external directory; fixed by adding the %dir entry for it.
- An earlier attempt dropped the FFmpeg %files entry as not installed by pip; that was wrong, since the data scheme does install it. The entry was restored at its real path.
- 50-check-packaged-twice: both Python flavors claimed the same FFmpeg files; fixed by gating those entries to the python314 flavor.
Checked against 2.0.4 on openSUSE_Tumbleweed, x86_64.
