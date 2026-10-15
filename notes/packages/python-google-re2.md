# python-google-re2 notes

## Build requirements
- The package is a compiled C++ extension, so the package is architecture-specific.
- re2-devel: provides the RE2 C++ headers and library the extension is built against.
- pybind11-devel: provides the pybind11 C++ headers on the default include path. The Python pybind11 module alone does not give setup.py an include directory, so the -devel package is needed.
Checked against 1.1.20251105 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Build could not find the pybind11 C++ headers. Fixed by adding the pybind11 -devel BuildRequires. Checked against 1.1.20251105 on openSUSE_Tumbleweed, x86_64.
