## Workarounds
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
Origin: pre-build review of the spec; the build succeeded with these in place. No build failure was logged for either item.
- The spec makes the cicc compiler in the wheel's nvvm/bin directory executable, so the installed compiler can run.
- Header files the spec removes from the wheel's include tree are also removed from the dist-info RECORD, so RECORD does not list files that are not installed.
- The License tag is SUSE-NonFree, because the upstream wheel ships NVIDIA's proprietary binaries. Not verified by a build result; taken from the spec as written.
