## Workarounds
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
Origin: pre-build review; no build failure was logged for either item.
- The spec makes the cicc compiler in the wheel's nvvm/bin directory executable, so the installed compiler can run.
- Header files the spec removes from the wheel's include tree are also removed from the dist-info RECORD, so RECORD does not list files that are not installed.
