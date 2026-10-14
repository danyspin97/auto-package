## Build requirements

- The package repackages the upstream wheel for the build architecture; it is not compiled from source here.
- The wheel's runtime dependencies (apache-tvm-ffi, nvidia-cutlass-dsl, tokenspeed-triton, torch) are listed as both BuildRequires and Requires.
- cuda-bindings is listed as both BuildRequires and Requires even though the wheel does not declare it: the package code imports it, so it must be added by hand.

Checked against 0.1.8 on openSUSE_Tumbleweed, x86_64.
