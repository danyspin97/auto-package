# cuda-cufile

## Test changes
- No upstream test suite is assumed, because the source is a prebuilt vendor archive. The pre-build review planned a spec comment before %files saying so. That reading was medium confidence and the archive contents were never listed, so it is unverified.
Checked against 1.19.1.55 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source0 is the prebuilt vendor archive for the build architecture. The package directory already holds the file under Source0's basename, so it is used as supplied and nothing is fetched. Its top-level layout is unverified because listing the archive was denied, so check it before relying on paths inside it.
Checked against 1.19.1.55 on openSUSE_Tumbleweed, x86_64.
