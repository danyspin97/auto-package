//! Zig dependency vendoring: a `vendor.tar.zst` for a package whose
//! `build.zig` fetches its dependencies, rebuilt from `build.zig.zon` by a
//! fixed step instead of by an agent. The step runs no zig and no foreign
//! code; it reads the manifest, fetches each dependency through the host
//! allowlist, checks its content hash and writes the archive.

mod fetch;
mod hash;
mod scratch;
mod tree;
mod unpack;
mod vendor_tar;
mod zon;

#[cfg(test)]
mod fetch_archive_tests;
#[cfg(test)]
mod fetch_tests;
#[cfg(test)]
mod hash_tests;
#[cfg(test)]
mod scratch_tests;
#[cfg(test)]
mod tree_tests;
#[cfg(test)]
mod unpack_meta_tests;
#[cfg(test)]
mod unpack_root_tests;
#[cfg(test)]
mod unpack_tests;
#[cfg(test)]
mod vendor_tar_tests;
#[cfg(test)]
mod zon_tests;
