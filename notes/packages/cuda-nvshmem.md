# cuda-nvshmem notes

## Build requirements

- BuildRequires fdupes: the `%fdupes` macro used in `%install` is only defined when fdupes is installed. Without it the macro stays unexpanded and the install step fails.
- BuildRequires patchelf: present since before the fdupes fix. Its exact use is not recorded. The RUNPATH rewrite planned in an early attempt was dropped, so check whether patchelf is still needed before relying on it.

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Upstream RUNPATHs are kept, not rewritten. Binaries and lib64 shared objects carry `$ORIGIN` or `$ORIGIN/../lib64`. rpmlint flags each one as binary-or-shlib-defines-rpath at 10000 badness, and the build fails once total badness exceeds 999, so the rpmlintrc filter for that rule is required.
- The rpmlintrc filter pattern must match the CUDA install prefix (the CUDA version directory under /usr/local) as the build produces it. Escaping the dot in the CUDA version made the filter match nothing, and rpmlint reported it as unused.
- Duplicate headers under `share/src` repeat `include/` headers. fdupes in `%install` hardlinks them to clear files-duplicate and files-duplicated-waste (about 555 KB).

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- rpmlint badness 783024 over threshold 999 from RUNPATH defines. Rewriting RUNPATHs to `$ORIGIN` did not clear it, because rpmlint flags those too. Fixed by keeping upstream RUNPATHs and filtering them.
- files-duplicated-waste and files-duplicate on `share/src` headers. Fixed by hardlinking duplicates with fdupes.
- `%fdupes` left unexpanded in `%install`. Fixed by adding BuildRequires fdupes.
- Unused rpmlintrc filter for the RUNPATH rule. Fixed by unescaping the dot in the CUDA version.
- A build log older than the last rpmlintrc edit showed the old filter as unused. Compare log and file times before trusting filter results.

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
