# python-chromadb notes

## Build requirements
- protobuf-devel: provides protoc, which the chroma-types crate's prost-build needs at build time. Without it the build script fails with "Could not find protoc". On openSUSE the package provides /usr/bin/protoc.
Checked against 1.5.9 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Quilt patch raises the rustc recursion limit to 256 in chroma-blockstore and the crates that depend on it, because async query layouts overflow the default depth. Upstream already sets this in the python bindings and sysdb crates.
- The Swagger UI archive is shipped as a source file and the utoipa-swagger-ui build is pointed at the local copy, because the build cannot download at build time.
- The vendored Cargo registry must match Cargo.lock. The cargo_vendor step must not run a cargo update, which pulled zip 9.x and broke zip-extract (locked to zip 8.3.0).
Checked against 1.5.9 on openSUSE_Tumbleweed, x86_64.

## Failure history
- "Could not find protoc" in the chroma-types build script: added protobuf-devel to BuildRequires.
- rustc recursion-limit overflow in chroma_blockstore: added the recursion-limit patch.
- utoipa-swagger-ui build script failed to download Swagger UI: supplied the archive as a source and pointed the build at it.
- zip-extract compile errors on zip API change: kept the vendored registry at the Cargo.lock versions.
- %files glob chromadb_rust_bindings*.so matched nothing, because the bindings install as a package directory: %files now lists the chromadb_rust_bindings directory.
- Embedding-function schemas would be missing at run time (found by reading the code, not from a build error): %files now ships site-packages/schemas/, which the maturin include in pyproject places there.
Checked against 1.5.9 on openSUSE_Tumbleweed, x86_64.
