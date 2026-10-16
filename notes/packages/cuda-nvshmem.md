# cuda-nvshmem notes

## Build requirements

- BuildRequires fdupes: the `%fdupes` macro used in `%install` is only defined when fdupes is installed. Without it the macro stays unexpanded and the install step fails.
- BuildRequires patchelf: already present before the fdupes fix. The journal does not record its exact use.

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Upstream RUNPATHs are kept, not rewritten. Binaries and lib64 shared objects carry `$ORIGIN` or `$ORIGIN/../lib64`. rpmlint flags each as binary-or-shlib-defines-rpath at 10000 badness, so the rpmlintrc filter must suppress them.
- The rpmlintrc filter pattern must match the CUDA install prefix the build actually produces. Escaping the dot in the CUDA version made the filter match nothing (reported as unused-rpmlintrc-filter).
- Duplicate headers under `share/src` repeat `include/` headers. fdupes in `%install` hardlinks them to clear files-duplicate and files-duplicated-waste (about 555 KB).
- rpmlint fails the build when total badness exceeds 999. Each binary-or-shlib-defines-rpath counts 10000, so the filter is required, not optional.

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- rpmlint badness 783024 over threshold 999 from RUNPATH defines. Rewriting RUNPATHs to `$ORIGIN` did not clear it; fixed by keeping upstream RUNPATHs and filtering them.
- files-duplicated-waste and files-duplicate on `share/src` headers; fixed by hardlinking duplicates with fdupes.
- `%fdupes` left unexpanded in `%install`; fixed by adding BuildRequires fdupes.
- Unused rpmlintrc filter for the RUNPATH rule; fixed by unescaping the dot in the CUDA version.

Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
