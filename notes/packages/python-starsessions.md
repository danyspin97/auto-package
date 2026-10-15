# python-starsessions notes

## Build requirements
- BuildRequires starlette and itsdangerous, so that the import check can load the package's dependencies. The review planned this; the journal does not show the exact Requires list.
- Runtime Requires: the review added the dependencies that were missing from the spec. Confirm the list against the package's declared dependencies before changing it.
Checked against 2.2.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Added a real %check in place of skipping tests. The journal does not record which tests it runs or whether any were deselected.
Checked against 2.2.1 on openSUSE_Tumbleweed, x86_64.
