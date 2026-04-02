//! Reusable Cyphal/UAVCAN node library for RP2040 projects using Embassy.
//!
//! This crate encapsulates the common Cyphal protocol functionality:
//! - PnP dynamic node-ID allocation
//! - `uavcan.node.GetInfo` service with configurable node information
//! - `uavcan.node.ExecuteCommand` service (Identify, Restart, FactoryReset,
//!   BeginSoftwareUpdate)
//! - OTA firmware update via `uavcan.file.Read`
//! - Heartbeat publishing (via `canadensis::node::BasicNode`)
//!
//! # Feature flags
//! - `mcp25xx` *(default)*: enables the MCP25625/MCP2515 CAN driver and the
//!   concrete [`run_cyphal_node`] async function for RP2040.
//! - `defmt`: pass-through feature for downstream `defmt` gating.
//!
//! # Quick start
//! See [`run_cyphal_node`] and [`NodeInfoConfig`] for the primary entry points.

#![no_std]

extern crate alloc;

#[cfg(feature = "mcp25xx")]
pub mod clock;
pub mod execute_command;
pub mod handler;
pub mod node_info;
pub mod ota;
pub mod pnp;

#[cfg(feature = "mcp25xx")]
pub mod driver;

#[cfg(feature = "mcp25xx")]
pub mod node_task;

// Re-exports — the public API surface.

#[cfg(feature = "mcp25xx")]
pub use clock::TimerClock;
pub use execute_command::{DefaultCommandHandler, PENDING_RESET, RESET_FACTORY, RESET_NONE, RESET_SOFT};
pub use handler::{CyphalHandler, NoopHandler};
pub use node_info::NodeInfoConfig;
pub use ota::OtaSession;
pub use pnp::{pnp_unique_id_hash, PnpHandler};

#[cfg(feature = "mcp25xx")]
pub use driver::Mcp25xxDriver;

#[cfg(feature = "mcp25xx")]
pub use node_task::{
    assigned_node_id, run_cyphal_node, FlashMutex, FlashType, SpiBusMutex, SpiBusType,
    ASSIGNED_NODE_ID,
};
