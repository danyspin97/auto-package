# python-nccl-extensions

Version 0.1.0. Last built successfully on the attempt-3 spec.

## Build requirements

- The package is built from the upstream per-CPython wheel, not the sdist. The wheel is `nccl_extensions-0.1.0-cp314-cp314-manylinux_2_24_x86_64.manylinux_2_28_x86_64.whl`, SHA256 `28f2513696bd03453857db32a5f6144631f874efb23b5fede4c6218e6026d6b1`.
- Why the wheel: the sdist's `python/setup.py` exits with "CUDA_HOME is not set" when CUDA is absent, and its Cython extensions include CUDA runtime headers (`driver_types.h`, `library_types.h`, `cuComplex.h`) from `nccl/_extensions/bindings/cynccl_ep.pxd` and `cynccl_m2n.pxd`. openSUSE does not ship the CUDA toolkit, so the source build cannot run here. Do not add cuda-devel or cuda-runtime.
- PyPI publishes 0.1.0 only as cp310 to cp314 and cp314t wheels (x86_64 and aarch64), with no noarch or sdist-only fallback. Each Python flavour therefore has its own wheel layout, and the spec currently handles python313 and python314.
- The package installs into the shared `nccl/` namespace (`nccl/_extensions`, `nccl/ep`, `nccl/m2n`), not into a `nccl_extensions/` directory.

## Test changes

None recorded.

## Workarounds

- Source switched to the upstream wheel (see above) because the CUDA toolkit is unavailable.
- `%files` lists the three `nccl` subtrees as directories, plus a `%dir` for the shared `nccl/` namespace, the dist-info directory for each Python, and a `%exclude`/`%license` pair for `licenses/LICENSE.txt`.
- rpmlintrc filters only the two devel-header warnings, for `nccl/ep/include` and `nccl/m2n/include` (matching both lib and lib64). The package has no -devel split.
- Summary was shortened to keep rpmlint badness under the 999 limit.
- Open: the spec's `%fdupes` targets `sitelib` while `%files` uses `sitearch`. The review flagged this, and the journal records no fix for it.

## Failure history

- Attempt 1: "Installed (but unpackaged) file(s)" for `nccl/_extensions`, `nccl/ep`, `nccl/m2n` and the dist-info. Fixed by filling `%files` with those directories, the dist-info, and the license pair.
- Attempt 3: rpmlint badness 1107 > 999 (summary-too-long at 3×200, devel headers at 10×50). Fixed by shortening Summary and cutting rpmlintrc to the two devel-header filters, removing five unused ones. The journal marks this attempt Medium confidence and notes that its log may predate the rpmlintrc edit, so the build result is confirmed but the rpmlint numbers may need a re-check.
