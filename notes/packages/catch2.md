# catch2 notes

## Build requirements

- BuildRequires cmake must be at least 3.16. Upstream's top-level CMakeLists.txt declares `cmake_minimum_required(VERSION 3.16)`, so older cmake cannot configure the build.

Checked against 3.16.1 on openSUSE_Factory, x86_64.
