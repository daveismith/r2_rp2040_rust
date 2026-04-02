//! OTA firmware update session state.
//!
//! [`OtaSession`] manages the state for a single Cyphal firmware-over-CAN
//! download. The actual drive logic (sending `file.Read` requests, processing
//! responses, writing flash) lives in [`crate::node_task`] where the concrete
//! hardware types are available.
//!
//! # Lifecycle
//! 1. `new()` → inactive session
//! 2. `start()` / `start_at_ticks()` → arms the session for a new download
//! 3. `drive_step` (called from the node task) → performs requests/responses
//! 4. `clear()` → resets all state (also called on abort or completion)

use canadensis_can::CanNodeId;
use canadensis_data_types::uavcan::file::path_2_0::Path as FilePath;
use embassy_time::{Duration, TICK_HZ};

/// Maximum serialised payload buffer for `file.Read` responses.
pub const READ_RESPONSE_PAYLOAD_MAX: usize = 300;
/// Number of firmware bytes requested per `file.Read` call.
pub const READ_CHUNK_SIZE: usize = 256;
/// Time to wait for a `file.Read` response before timing out.
pub const OTA_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum consecutive `file.Read` timeouts before aborting.
pub const OTA_TIMEOUT_RETRY_LIMIT: u8 = 8;
/// Log a progress message every this many downloaded bytes.
pub const OTA_PROGRESS_LOG_STEP: usize = 16 * 1024;

/// State machine for a single OTA firmware download session.
///
/// All fields are `pub(crate)` so that [`crate::node_task::drive_ota_step`]
/// can read/write them without exposing internals to downstream crates.
pub struct OtaSession {
    /// Whether a download is currently in progress.
    pub active: bool,
    pub(crate) server_node: CanNodeId,
    pub(crate) path: heapless::Vec<u8, 255>,
    /// Next byte offset to request from the file server.
    pub next_offset: usize,
    /// Total `file.Read` requests sent in this session.
    pub requests_sent: u32,
    /// Total `file.Read` responses received.
    pub responses_received: u32,
    /// Total 256-byte chunks written to flash.
    pub chunks_written: u32,
    pub(crate) start_tick: u64,
    pub(crate) wait_started_tick: u64,
    pub(crate) total_wait_ticks: u64,
    pub(crate) total_write_ticks: u64,
    pub(crate) next_progress_log_at: usize,
    /// Whether we are waiting for the response to the most recent request.
    pub waiting_response: bool,
    /// Absolute Embassy tick at which the current request times out.
    pub(crate) response_deadline_ticks: u64,
    /// Number of consecutive response timeouts.
    pub response_timeouts: u8,
    /// Payload buffered by the `TransferHandler` when a `file.Read` response arrives.
    pub pending_response_payload: Option<heapless::Vec<u8, READ_RESPONSE_PAYLOAD_MAX>>,
}

impl OtaSession {
    /// Create a new, inactive OTA session.
    pub fn new() -> Self {
        Self {
            active: false,
            server_node: CanNodeId::MIN,
            path: heapless::Vec::new(),
            next_offset: 0,
            requests_sent: 0,
            responses_received: 0,
            chunks_written: 0,
            start_tick: 0,
            wait_started_tick: 0,
            total_wait_ticks: 0,
            total_write_ticks: 0,
            next_progress_log_at: OTA_PROGRESS_LOG_STEP,
            waiting_response: false,
            response_deadline_ticks: 0,
            response_timeouts: 0,
            pending_response_payload: None,
        }
    }

    /// Arm the session, using an explicit start tick (useful for testing).
    ///
    /// Returns `Err(())` if:
    /// - a download is already active
    /// - `path_bytes` is empty or longer than [`FilePath::MAX_LENGTH`]
    pub fn start_at_ticks(
        &mut self,
        server_node: CanNodeId,
        path_bytes: &[u8],
        start_ticks: u64,
    ) -> Result<(), ()> {
        if self.active {
            return Err(());
        }
        if path_bytes.is_empty() || path_bytes.len() > FilePath::MAX_LENGTH as usize {
            return Err(());
        }
        let mut path = heapless::Vec::<u8, 255>::new();
        if path.extend_from_slice(path_bytes).is_err() {
            return Err(());
        }
        self.active = true;
        self.server_node = server_node;
        self.path = path;
        self.reset_transfer_state(start_ticks);
        Ok(())
    }

    /// Arm the session using the current Embassy clock for the start timestamp.
    ///
    /// In `#[test]` builds the start tick is forced to `0`; use
    /// [`start_at_ticks`] directly when you need explicit tick control.
    pub fn start(&mut self, server_node: CanNodeId, path_bytes: &[u8]) -> Result<(), ()> {
        #[cfg(not(test))]
        let now_ticks = embassy_time::Instant::now().as_ticks();
        #[cfg(test)]
        let now_ticks = 0u64;
        self.start_at_ticks(server_node, path_bytes, now_ticks)
    }

    /// Reset all state to inactive.
    pub fn clear(&mut self) {
        self.active = false;
        self.server_node = CanNodeId::MIN;
        self.path.clear();
        self.reset_transfer_state(0);
    }

    pub(crate) fn reset_transfer_state(&mut self, start_tick: u64) {
        self.next_offset = 0;
        self.requests_sent = 0;
        self.responses_received = 0;
        self.chunks_written = 0;
        self.start_tick = start_tick;
        self.wait_started_tick = 0;
        self.total_wait_ticks = 0;
        self.total_write_ticks = 0;
        self.next_progress_log_at = OTA_PROGRESS_LOG_STEP;
        self.waiting_response = false;
        self.response_deadline_ticks = 0;
        self.response_timeouts = 0;
        self.pending_response_payload = None;
    }

    pub(crate) fn accumulate_wait_ticks(&mut self, now_ticks: u64) {
        if self.wait_started_tick != 0 {
            self.total_wait_ticks = self
                .total_wait_ticks
                .saturating_add(now_ticks.saturating_sub(self.wait_started_tick));
        }
        self.wait_started_tick = 0;
    }

    pub(crate) fn ticks_to_ms(ticks: u64) -> u64 {
        if TICK_HZ > 0 {
            ticks.saturating_mul(1000) / TICK_HZ
        } else {
            0
        }
    }

    pub(crate) fn abort(&mut self, reason: &str) {
        log::warn!(
            "ota: abort reason='{}' downloaded={} req={} resp={}",
            reason,
            self.next_offset,
            self.requests_sent,
            self.responses_received
        );
        self.clear();
    }
}

impl Default for OtaSession {
    fn default() -> Self {
        Self::new()
    }
}

