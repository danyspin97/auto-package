# python-nccl-extensions

Version 0.1.0. Last built successfully on openSUSE_Tumbleweed, x86_64.

## Build requirements

- Built from the upstream per-CPython wheel for the build architecture, not the sdist. The sdist's setup.py exits with "CUDA_HOME is not set" when CUDA is absent, and its Cython extensions include CUDA runtime headers from the bindings .pxd files. Distros without the CUDA toolkit cannot compile it, so do not add cuda-devel or cuda-runtime.
- Requires the NCCL runtime package (nvidia-nccl-cu13) at run time, because the wheel's libraries need the NCCL soname.
- Runtime Requires on cuda-core and cuda-pathfinder carry the upstream bounds from the sdist's [project.dependencies] (cuda-core ~=1.0, cuda-pathfinder >=1.5.4,<2.0.0). Keep them in step with upstream.
- PyPI publishes 0.1.0 as per-CPython wheels (cp310 to cp314, and cp314t), one set per architecture, with no sdist fallback that builds without CUDA. Each Python flavour needs its own Source entry and file list.
- The wheel installs into the shared nccl/ namespace (nccl/_extensions, nccl/ep, nccl/m2n), not into a nccl_extensions/ directory.
- The flavours built are those in the distro's %pythons macro. On openSUSE_Tumbleweed that is python313 and python314.

Checked against 0.1.0 on openSUSE_Tumbleweed, x86_64.

## Test changes

- %check no longer tests for a library file. It runs an import smoke test of the nccl.ep and nccl.m2n facades for each flavour under %python_expand, because those are the public import paths and the package adds no nccl/__init__.py.

Checked against 0.1.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- %files lists the three nccl subtrees as directories, a %dir for the shared nccl/ namespace, the dist-info directory for each flavour, and the license pair. Unlisted files fail the build as unpackaged.
- rpmlintrc filters only the two devel-header warnings (nccl/ep/include and nccl/m2n/include, lib and lib64). The package has no -devel split.
- Summary is kept short so rpmlint badness stays under 999.
- Open: %fdupes targets sitelib while %files uses sitearch. The journal records no fix.

Checked against 0.1.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Earlier attempt 1: unpackaged nccl/_extensions, nccl/ep, nccl/m2n and dist-info. Fixed by listing them in %files.
- Earlier attempt 3: rpmlint badness 1107 over the 999 limit (long summary, devel headers). Fixed by shortening Summary and keeping two rpmlintrc filters. Badness was not re-measured in the later successful build.

Checked against 0.1.0 on openSUSE_Tumbleweed, x86_64.
