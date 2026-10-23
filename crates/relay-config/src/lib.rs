//! Relay config engine: lossless parsing and patching of JSON, JSONC, TOML,
//! YAML and `.env` files, plus diff, backup and rollback.
//!
//! Contract: a patch touches only the spans it must. Comments, key order,
//! formatting and unknown fields survive every Relay edit — the user's file
//! stays theirs.

pub mod atomic;
pub mod backup;
pub mod document;
pub mod env_edit;
pub mod json_edit;
pub mod kit;
pub mod path;
pub mod perms;
pub mod toml_edit;
pub mod yaml_edit;

pub use atomic::{read_file, write_file};
pub use backup::{file_hash, BackupFile, BackupRecord, BackupStore};
pub use document::Document;
pub use path::ConfigPath;
pub use relay_core::transport::unified_diff;
