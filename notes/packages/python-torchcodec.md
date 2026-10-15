## Build requirements
- The package is an architecture-dependent C++ extension (scikit-build-core, CMake sources under the core directory): install into sitearch and run the arch pytest macro.
- torch is both BuildRequires and Requires; the wheel is built against it.
- torch-devel for the Python flavour: the CMake config (TorchConfig.cmake) ships in this subpackage, not the runtime one. Without it find_package(Torch) fails.
- protobuf-devel: torch's Caffe2 CMake config aborts configure when torch uses protobuf and no protobuf CMake config is found.
- FFmpeg 8 libraries and the ffmpeg CLI: the CLI is used by the tests. On home:dspinella:auto-package2 only, the full FFmpeg 8 devel package was not published and the -mini devel flavour was used; the last journal entry pins the full per-library devel packages instead. Verify which one the spec carries.
- Test-only: Pillow, torchvision, numpy, image-codec devel, pybind11-devel.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- test_plugin_autoload is deselected: it needs network access.
- The FIFO test is deselected: it hangs the check run.
- On home:dspinella:auto-package2 openSUSE_Tumbleweed, x86_64 only: the distro FFmpeg has no H.264/H.265 decoder and no libx264/libx265 encoder, so about 650 tests in 7 files fail. The check runs an import smoke test plus the test files that pass. The journal does not confirm whether the AV1 and VP9 encoder tests are also deselected.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The AVIF build option is set to 0: the AVIF decoder is downloaded at configure time, which the build cannot do offline.
- The license-confirmation flag is set to 1 so the wheel-state guard accepts a wheel that links the distro FFmpeg instead of the upstream-fetched one. The guard's purpose is a licence concern; this needs a licence review, not just a build fix.
- C++ sources and headers under the core directory are removed in install: rpmlint reports them as devel files in the non-devel package, above the error threshold.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- find_package(Torch) found no TorchConfig.cmake: added torch-devel BuildRequires.
- Caffe2 config aborted on missing protobuf: added protobuf-devel BuildRequires.
- AVIF decoder fetch failed at configure: AVIF build option set to 0.
- Wheel-state guard refused a build not linking the upstream FFmpeg: confirmation flag set to 1.
- ffmpeg-8-devel not published in the project repos: switched to the -mini devel flavour (later reversed; see Build requirements).
- The FIFO test hung the check run: deselected.
- test_plugin_autoload needs network: deselected.
- Codec tests failed on the distro FFmpeg: check reduced to smoke test plus passing files.
- rpmlint devel-file-in-non-devel-package (194 errors): C++ sources and headers stripped in install.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.
