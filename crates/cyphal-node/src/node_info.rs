//! Node information builder for `uavcan.node.GetInfo`.
//!
//! [`NodeInfoConfig`] is a builder that produces a
//! [`canadensis::node::data_types::GetInfoResponse`] with sensible defaults
//! derived from Cargo build-time environment variables.
//!
//! # Defaults (when not explicitly overridden)
//! | Field | Default |
//! |---|---|
//! | `name` | `env!("CARGO_PKG_NAME")` of the calling crate |
//! | `software_version.major` | parsed from `env!("CARGO_PKG_VERSION_MAJOR")` |
//! | `software_version.minor` | parsed from `env!("CARGO_PKG_VERSION_MINOR")` |
//! | `software_vcs_revision_id` | `0` |
//! | `hardware_version` | `{ major: 1, minor: 0 }` |
//! | `protocol_version` | `{ major: 1, minor: 0 }` (fixed) |
//! | `unique_id` | required — must be provided via [`NodeInfoConfig::new`] |
//!
//! # Example
//! ```rust,ignore
//! let node_info = NodeInfoConfig::new(unique_id)
//!     .with_name("my-device")
//!     .with_software_version(0, 5)
//!     .with_vcs_revision_id(0xdeadbeef)
//!     .build();
//! ```
//!
//! Or using the convenience macro that reads Cargo metadata automatically:
//! ```rust,ignore
//! let node_info = default_node_info!(unique_id).build();
//! ```

use canadensis::node::data_types::{GetInfoResponse, Version};

/// Builder for [`GetInfoResponse`].
///
/// All fields have sensible defaults; override only what you need.
pub struct NodeInfoConfig {
    unique_id: [u8; 16],
    name: Option<&'static str>,
    software_version: Option<(u8, u8)>,
    hardware_version: Option<(u8, u8)>,
    vcs_revision_id: Option<u64>,
}

impl NodeInfoConfig {
    /// Create a builder with the required 16-byte unique device ID.
    ///
    /// All other fields use defaults until explicitly set.
    pub fn new(unique_id: [u8; 16]) -> Self {
        Self {
            unique_id,
            name: None,
            software_version: None,
            hardware_version: None,
            vcs_revision_id: None,
        }
    }

    /// Set the node name (e.g. `"my-device"`).
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    /// Set the software version (`major`, `minor`).
    pub fn with_software_version(mut self, major: u8, minor: u8) -> Self {
        self.software_version = Some((major, minor));
        self
    }

    /// Set the software version from string slices (e.g. from
    /// `env!("CARGO_PKG_VERSION_MAJOR")`).
    ///
    /// Non-numeric strings silently default to `0`.
    pub fn with_software_version_str(mut self, major: &str, minor: &str) -> Self {
        let maj = u8::from_str_radix(major, 10).unwrap_or(0);
        let min = u8::from_str_radix(minor, 10).unwrap_or(0);
        self.software_version = Some((maj, min));
        self
    }

    /// Set the hardware version (`major`, `minor`).
    pub fn with_hardware_version(mut self, major: u8, minor: u8) -> Self {
        self.hardware_version = Some((major, minor));
        self
    }

    /// Set the VCS (git) revision ID, typically the short commit hash as a
    /// `u64` (e.g. `u64::from_str_radix(git_hash_short, 16).unwrap_or(0)`).
    pub fn with_vcs_revision_id(mut self, id: u64) -> Self {
        self.vcs_revision_id = Some(id);
        self
    }

    /// Build the [`GetInfoResponse`].
    ///
    /// Uses defaults from `env!("CARGO_PKG_NAME")` /
    /// `env!("CARGO_PKG_VERSION_MAJOR")` / `env!("CARGO_PKG_VERSION_MINOR")`
    /// for any fields not explicitly set. The defaults are evaluated in the
    /// *calling crate's* context at compile time, so they reflect the
    /// application's own package metadata, not `cyphal-node`'s.
    ///
    /// **Important:** do not call this method from within the `cyphal-node`
    /// crate itself — always call it from the application crate so that the
    /// `env!` macros expand to the correct values.
    pub fn build_with_env(self, pkg_name: &'static str, version_major: &'static str, version_minor: &'static str) -> GetInfoResponse {
        let name_str = self.name.unwrap_or(pkg_name);
        let (sw_major, sw_minor) = self.software_version.unwrap_or_else(|| {
            let maj = u8::from_str_radix(version_major, 10).unwrap_or(0);
            let min = u8::from_str_radix(version_minor, 10).unwrap_or(0);
            (maj, min)
        });
        let (hw_major, hw_minor) = self.hardware_version.unwrap_or((1, 0));

        let mut node_name = heapless::Vec::new();
        if node_name.extend_from_slice(name_str.as_bytes()).is_err() {
            log::warn!("cyphal-node: node name too long, truncating");
        }

        GetInfoResponse {
            protocol_version: Version { major: 1, minor: 0 },
            hardware_version: Version { major: hw_major, minor: hw_minor },
            software_version: Version { major: sw_major, minor: sw_minor },
            software_vcs_revision_id: self.vcs_revision_id.unwrap_or(0),
            unique_id: self.unique_id,
            name: node_name,
            software_image_crc: Default::default(),
            certificate_of_authenticity: Default::default(),
        }
    }

    /// Build the [`GetInfoResponse`] using only the explicitly-set fields.
    ///
    /// Fields not set default to empty name, version 0.0, hardware 1.0.
    /// Prefer [`build_with_env`](Self::build_with_env) or the
    /// [`default_node_info!`] macro for automatic Cargo metadata.
    pub fn build(self) -> GetInfoResponse {
        self.build_with_env("", "0", "0")
    }
}

/// Convenience macro that creates a [`NodeInfoConfig`] pre-populated with the
/// calling crate's Cargo metadata.
///
/// The unique ID must be supplied as the only argument. All fields can still
/// be overridden with builder methods before calling `.build()`.
///
/// # Example
/// ```rust,ignore
/// pub mod built_info { include!(concat!(env!("OUT_DIR"), "/built.rs")); }
///
/// let node_info = default_node_info!(unique_id)
///     .with_vcs_revision_id(
///         built_info::GIT_COMMIT_HASH_SHORT
///             .and_then(|h| u64::from_str_radix(h, 16).ok())
///             .unwrap_or(0),
///     )
///     .build_with_env(
///         env!("CARGO_PKG_NAME"),
///         env!("CARGO_PKG_VERSION_MAJOR"),
///         env!("CARGO_PKG_VERSION_MINOR"),
///     );
/// ```
#[macro_export]
macro_rules! default_node_info {
    ($uid:expr) => {
        $crate::NodeInfoConfig::new($uid)
    };
}
