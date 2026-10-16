## Build requirements
- The package is an architecture-dependent C++ extension (scikit-build-core, CMake sources under the core directory): install into sitearch.
- torch is both BuildRequires and Requires: the wheel is built against it, and the project declares no runtime dependencies.
- torch-devel: TorchConfig.cmake ships in this subpackage, not the runtime one; find_package(Torch) fails without it.
- protobuf-devel: torch's Caffe2 CMake config aborts configure when torch uses protobuf and no protobuf CMake config is found.
- pybind11-devel: CMake takes pybind11 from this package's config. The pip pybind11 package is noarch, and the path derived from it expanded to the default flavour's lib64 directory, so it is not used.
- ffmpeg-8: the FFmpeg 8 libraries and the ffmpeg CLI; the tests call the CLI.
- Test-only: torchvision (test collection imports its v2 transforms), Pillow, numpy, image-codec devel.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- test_plugin_autoload is deselected: it needs network access.
- The FIFO test is deselected: it hangs the check run.
- On openSUSE_Tumbleweed only: the distro FFmpeg has no H.264/H.265 decoder and no x264/x265 encoder. Before the check was narrowed, 765 failures came from decoder, encoder, transform-ops, sampler, ops, smoke and metadata tests, plus logging tests. The check now runs an import smoke test and the test files that do not need those codecs. The spec holds the exact list; the journal does not record it.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The AVIF build option is set to 0: the AVIF decoder is downloaded at configure time, which OBS cannot do.
- The CMake wheel-state guard aborts every wheel build unless a license-confirmation variable or a fetch-FFmpeg variable is set. The fetch-FFmpeg option needs network, so the license-confirmation variable is set in %build instead. %build also checks that the distro FFmpeg was not configured with GPL or nonfree options. This is a license decision that needs a license review, not just a build fix.
- Upstream warnings-as-errors is disabled: LTO reported maybe-uninitialized warnings in the GIF decoder that failed the image library link. On openSUSE_Tumbleweed only.
- %check does not export a PYTHONPATH built from %{python_sitearch}: the macro expands for the default flavour only, so the other flavour's pytest imports an extension built for the wrong Python.
- C++ sources and headers under the core directory are removed in install: rpmlint reports them as devel files in the non-devel package, above the error threshold.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- find_package(Torch) found no TorchConfig.cmake: added torch-devel.
- Caffe2 config aborted without a protobuf CMake config: added protobuf-devel.
- AVIF decoder fetch failed at configure: AVIF option set to 0.
- Wheel-state guard refused a build not linking the upstream FFmpeg: license confirmation set in %build, backed by a build-time FFmpeg configuration check.
- pybind11 pip CMake path pointed at the default flavour's directory: switched to pybind11-devel.
- LTO link failed on maybe-uninitialized warnings under warnings-as-errors: upstream warnings-as-errors disabled.
- %check PYTHONPATH made the other flavour import a 3.13 extension: removed the export.
- Test collection failed with ModuleNotFoundError for torchvision: added torchvision test BuildRequires.
- The FIFO test hung the check run: deselected.
- test_plugin_autoload needs network: deselected.
- Codec tests failed on the distro FFmpeg: check narrowed to smoke test plus codec-independent files.
- rpmlint devel-file-in-non-devel-package (194 errors): C++ sources and headers stripped in install.
Checked against 0.17.0 on openSUSE_Tumbleweed, x86_64.
