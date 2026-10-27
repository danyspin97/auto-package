# dqlite

## Build requirements
- BuildRequires libuv >= 1.34.0 and sqlite3 >= 3.34.0. These are the minimums that the upstream configure.ac checks with pkg-config, so older versions make configure fail.
- configure.ac also requires liblz4 >= 1.7.1; the spec's liblz4 BuildRequires should not be older than that.
Checked against 1.18.7 on openSUSE_Factory, x86_64.
