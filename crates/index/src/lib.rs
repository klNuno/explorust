//! explorust's own file index: every name of every fixed NTFS volume, read from the MFT
//! (`FSCTL_ENUM_USN_DATA`) and kept current from the USN journal (`FSCTL_READ_USN_JOURNAL`) by the
//! elevated service `explorust-index` (`src/bin/explorust-index`), which answers prefix and
//! substring queries over the named pipe `\\.\pipe\explorust-index`. Start asks it on every key
//! through [`client`] and merges its hits with the Windows Search index.
//!
//! The pure parts (name table, folding, ranking, wire format, USN record parsing) build and test
//! on any OS; the volume, pipe and service code is Windows only.

pub mod fold;
pub mod proto;
pub mod table;
pub mod usn;

#[cfg(target_os = "windows")]
pub mod client;

/// The pipe the service listens on.
pub const PIPE: &str = r"\\.\pipe\explorust-index";
/// The service name `explorust-guard --install` registers.
pub const SERVICE: &str = "explorust-index";
