## Build requirements

- The package repackages the upstream wheel for the build architecture; it is not compiled from source here.
- The wheel's declared runtime dependencies are listed as both BuildRequires and Requires, with the wheel's version floors: apache-tvm-ffi (at least 0.1.11, below 0.2), nvidia-cutlass-dsl (at least 4.8.0), tokenspeed-triton (at least 3.8.10.post20260920) and torch.
- The nvidia-cutlass-dsl floor moved from 4.7.1 (0.1.8) to 4.8.0 (0.2.15). Take the floor from the wheel's metadata on each version bump, not from the previous spec.

Checked against 0.2.15 on openSUSE_Tumbleweed, x86_64.
