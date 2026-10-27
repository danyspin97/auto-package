# aws-c-s3 notes

## Build requirements

- The runtime library subpackage is named after the upstream soversion (lib%{name}<major>_<minor>). When the soversion changes, rename the subpackage to match and update any dependents. For 1.4.x the name is the 1.4 one, replacing the 1.3 name. The 1.4.1 build succeeded with this name.

Checked against 1.4.1 on openSUSE_Factory, x86_64.
