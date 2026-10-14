# python-torch_c_dlpack_ext

## Build requirements
- BuildRequires apache-tvm-ffi (>= 0.1.1), torch, packaging and gcc-c++. The build backend imports tvm_ffi at build time. It compiles a native library through a tvm_ffi subprocess when torch lacks the DLPack attribute, which is why a C++ compiler is needed.
- Requires torch and packaging. The source imports both at runtime, but pyproject declares only torch, so packaging must be listed by hand.
- Not noarch. The build can compile a native library, so the RPM must be built per architecture.
- No CUDA build dependency. The CUDA path runs only when torch.cuda is available at build time, which build workers do not provide.
Checked against 0.1.5 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist ships no tests. %check runs an import smoke test of the package instead.
Checked against 0.1.5 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- License tag: the upstream license field holds the full Apache text, so the spec uses the SPDX identifier Apache-2.0.
- Stray build helper: the wheel contains build_backend.py with its pycache and RECORD entries. They are removed in %install because they are not part of the package.
- Install location: the wheel has no extension modules, so the package installs under python_sitelib.
Checked against 0.1.5 on openSUSE_Tumbleweed, x86_64.
