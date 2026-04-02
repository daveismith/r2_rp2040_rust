//! Cyphal PnP v1 node-ID allocation support.
//!
//! Implements the client side of the UAVCAN PnP (Plug-and-Play) v1 protocol
//! ([UAVCAN Specification §6.1]), which allows nodes to obtain a dynamic node
//! ID without prior configuration.
//!
//! # Usage
//! 1. Create a [`PnpHandler`] with the device's 16-byte unique ID.
//! 2. Subscribe to the PnP allocation subject on an anonymous `CoreNode`.
//! 3. Periodically publish [`PnpMsg`] requests.
//! 4. Pass the handler to `core.receive(&mut pnp_handler)` on each tick.
//! 5. Poll [`PnpHandler::assigned_id`] — once set the allocation is complete.

use canadensis::TransferHandler;
use canadensis_can::CanTransport;
use canadensis_data_types::uavcan::pnp::node_id_allocation_data_1_0 as pnp_v1;
use canadensis_data_types::uavcan::pnp::node_id_allocation_data_1_0::NodeIDAllocationData as PnpMsg;
use canadensis::encoding::Deserialize;
use canadensis_can::CanNodeId;
use embassy_time::Duration;

/// Minimum inter-request delay (ms).
pub const PNP_RETRY_MIN_MS: u64 = 150;
/// Maximum additional jitter (ms) added on top of `PNP_RETRY_MIN_MS`.
pub const PNP_RETRY_JITTER_MS: u64 = 850;
/// Short timeout used when polling the interrupt pin for incoming PnP replies.
pub(crate) const PNP_POLL_TIMEOUT: Duration = Duration::from_millis(20);
/// Sleep duration between PnP polling iterations.
pub(crate) const LOOP_SLEEP_IDLE: Duration = Duration::from_millis(1);

/// Compute the CRC-64/WE hash of a 16-byte unique ID, keeping the lowest 48
/// bits. This matches the hash algorithm used by a compliant PnP allocator
/// server.
pub fn pnp_unique_id_hash(unique_id: &[u8; 16]) -> u64 {
    use crc_any::CRCu64;
    let mut crc = CRCu64::crc64we();
    crc.digest(unique_id);
    crc.get_crc() & 0x0000_ffff_ffff_ffff
}

/// Compute a deterministic per-node retry delay for PnP allocation requests.
///
/// Returns a [`Duration`] in the range
/// `[PNP_RETRY_MIN_MS, PNP_RETRY_MIN_MS + PNP_RETRY_JITTER_MS)`.
/// The delay depends on both the device's unique ID and the attempt number,
/// so different devices get different backoff values.
pub fn pnp_request_retry_duration(unique_id: &[u8; 16], attempt: u32) -> Duration {
    let mut state = u32::from_le_bytes([unique_id[0], unique_id[1], unique_id[2], unique_id[3]])
        ^ u32::from_le_bytes([unique_id[4], unique_id[5], unique_id[6], unique_id[7]])
        ^ attempt.wrapping_mul(0x9E37_79B9);
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    let jitter_ms = (state as u64) % PNP_RETRY_JITTER_MS;
    Duration::from_millis(PNP_RETRY_MIN_MS + jitter_ms)
}

/// Cyphal PnP v1 allocation response handler.
///
/// Receives `uavcan.pnp.NodeIDAllocationData` messages on the anonymous bus
/// and records the first node ID assigned to this device's unique-ID hash.
pub struct PnpHandler {
    /// CRC-64/WE hash of this device's 16-byte unique ID (lowest 48 bits).
    pub unique_id_hash: u64,
    /// Set to `Some(id)` when a matching allocation response is received.
    pub assigned_id: Option<CanNodeId>,
}

impl PnpHandler {
    /// Create a new handler for the given 16-byte device unique ID.
    pub fn new(unique_id: &[u8; 16]) -> Self {
        Self {
            unique_id_hash: pnp_unique_id_hash(unique_id),
            assigned_id: None,
        }
    }
}

impl TransferHandler<CanTransport> for PnpHandler {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        transfer: &canadensis::core::transfer::MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.subject != pnp_v1::SUBJECT {
            return false;
        }
        // Rule C: restart our timer on any allocation message (handled externally).
        // Rule D: accept only non-anonymous sources that match our hash and
        //         carry a node ID.
        if transfer.header.source.is_none() {
            // Anonymous source → this is a request from another allocatee, not
            // a response from the allocator server.
            return true;
        }
        let Ok(msg) = PnpMsg::deserialize_from_bytes(&transfer.payload) else {
            return true;
        };
        if msg.unique_id_hash != self.unique_id_hash {
            return true;
        }
        if let Some(id_entry) = msg.allocated_node_id.iter().next() {
            if let Ok(node_id) = CanNodeId::try_from(id_entry.value as u8) {
                self.assigned_id = Some(node_id);
            }
        }
        true
    }
}
