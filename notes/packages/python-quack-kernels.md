# python-quack-kernels

## Build requirements
- The upstream declared runtime dependencies are Requires and BuildRequires. numpy, triton and cuda-bindings are added explicitly because quack imports them without declaring them upstream.
- nvidia-cutlass-dsl-libs-core and nvidia-cutlass-dsl-libs-base are both BuildRequires and Requires. The cutlass package is split across them: libs-core ships the pure-Python cutlass/__init__.py and cutlass/cutlass_dsl/, and libs-base ships only the compiled cutlass/_mlir parts in the architecture library directory. Without both, the %check import fails with cutlass.cutlass_dsl missing, even with nvidia-cutlass-dsl installed.
- Do not depend on a bare "cutlass" package name; the import name belongs to nvidia-cutlass-dsl.
- Requires use the flavor-neutral python-<name> form so each Python flavor does not pull in the other flavor's packages. Reasoning only; not separately confirmed.
Checked against 0.6.5 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The GPU-only %pytest is replaced by an import smoke test of quack, because the GPU tests cannot run in the build environment.
Checked against 0.6.5 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- %check builds a merged cutlass tree per Python flavor and imports quack against it. The libs-core tree is linked in first, then the libs-base tree is linked in without overwriting existing entries. Reason: the two RPMs install into different library trees, and whether the nvidia_cutlass_dsl .pth file adds both was never verified.
- Each flavor needs its own merge directory; a shared one fails with File exists errors.
- Keep each flavor's merge-and-import step on one self-contained line. A multi-line block made the steps run for every flavor into the same directory.
- If cutlass._mlir fails to import again, the split layout is a packaging bug in the cutlass-dsl-libs RPMs; report it upstream rather than adding more workarounds.
Checked against 0.6.5 on openSUSE_Tumbleweed, x86_64.

## Failure history
- %check: cutlass.cutlass_dsl missing → added libs-base; not sufficient on its own.
- Still missing → added libs-core, which ships the pure-Python cutlass files.
- Both dsl_packages directories on PYTHONPATH → superseded by the tree merge above.
- Shared merge directory across flavors gave File exists → one directory per flavor, no-overwrite linking for the second tree.
- Block-form %check looped steps over flavors → one line per flavor.
Checked against 0.6.5 on openSUSE_Tumbleweed, x86_64.
