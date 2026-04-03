//! `uavcan.node.ExecuteCommand` service handler.
//!
//! Provides the standard built-in commands:
//! - `COMMAND_IDENTIFY` (65529) — triggers the identify LED callback
//! - `COMMAND_RESTART` (65535) — schedules a soft reset
//! - `COMMAND_FACTORY_RESET` (65532) — schedules a factory-reset (NVS erase + reboot)
//! - `COMMAND_BEGIN_SOFTWARE_UPDATE` (65533) — starts an OTA session
//!
//! The [`DefaultCommandHandler`] is wired into [`crate::handler::CyphalHandler`]
//! and called whenever an `ExecuteCommand` service request is received.
//!
//! Application-specific commands can be handled by providing a custom
//! extension [`canadensis::TransferHandler`] to [`crate::handler::CyphalHandler`].

use canadensis_can::CanNodeId;
use canadensis_data_types::uavcan::file::path_2_0::Path as FilePath;
use canadensis_data_types::uavcan::node::execute_command_1_3::{
    ExecuteCommandRequest, ExecuteCommandResponse,
};
use embassy_time::Duration;
use portable_atomic::{AtomicU8, Ordering};

use crate::ota::OtaSession;

/// Pending-reset state: no reset requested.
pub const RESET_NONE: u8 = 0;
/// Pending-reset state: soft reboot requested.
pub const RESET_SOFT: u8 = 1;
/// Pending-reset state: factory-reset (NVS erase + reboot) requested.
pub const RESET_FACTORY: u8 = 2;

/// Global pending-reset flag, written by [`DefaultCommandHandler`] and polled
/// by the node's main loop in [`crate::node_task::run_cyphal_node`].
pub static PENDING_RESET: AtomicU8 = AtomicU8::new(RESET_NONE);

/// Function pointer type for the "identify" callback.
///
/// The callback receives the duration for which the device should visually
/// identify itself (e.g. blink an LED).
pub type IdentifyCallback = fn(Duration);

/// Handler for the four standard `uavcan.node.ExecuteCommand` commands.
///
/// # Wiring the identify callback
/// Pass `Some(identify_led::trigger_identify)` (or any `fn(Duration)`) as
/// `identify_cb` to enable visual identification. Pass `None` to ignore the
/// command (it still responds `STATUS_SUCCESS`).
///
/// # NVS range
/// `nvs_range` is the flash address range erased during `COMMAND_FACTORY_RESET`.
/// Typically `0x480000..0x500000` for the RP2040 feather layout.
pub struct DefaultCommandHandler {
    /// Optional callback invoked on `COMMAND_IDENTIFY`.
    pub identify_cb: Option<IdentifyCallback>,
    /// Flash address range erased on `COMMAND_FACTORY_RESET`.
    pub nvs_range: core::ops::Range<u32>,
}

impl DefaultCommandHandler {
    /// Create a new handler.
    ///
    /// - `identify_cb`: optional LED-blink callback.
    /// - `nvs_range`: flash range to erase on factory-reset.
    pub fn new(identify_cb: Option<IdentifyCallback>, nvs_range: core::ops::Range<u32>) -> Self {
        Self { identify_cb, nvs_range }
    }

    /// Process an `ExecuteCommand` request.
    ///
    /// Returns `Some((status, output))` for standard commands that were
    /// handled, or `None` for unrecognised commands so the caller can
    /// delegate to a vendor-specific extension handler.
    ///
    /// The `ota` session is needed to start/reject `BEGIN_SOFTWARE_UPDATE`.
    pub(crate) fn handle(
        &mut self,
        request: &ExecuteCommandRequest,
        source_node: CanNodeId,
        ota: &mut OtaSession,
    ) -> Option<(u8, &'static [u8])> {
        match request.command {
            ExecuteCommandRequest::COMMAND_IDENTIFY => {
                log::info!("ExecuteCommand: IDENTIFY");
                if let Some(cb) = self.identify_cb {
                    cb(Duration::from_secs(5));
                }
                Some((ExecuteCommandResponse::STATUS_SUCCESS, b"identify"))
            }
            ExecuteCommandRequest::COMMAND_RESTART => {
                log::info!("ExecuteCommand: RESTART");
                PENDING_RESET.store(RESET_SOFT, Ordering::Release);
                Some((ExecuteCommandResponse::STATUS_SUCCESS, b"restart"))
            }
            ExecuteCommandRequest::COMMAND_FACTORY_RESET => {
                log::info!("ExecuteCommand: FACTORY_RESET");
                PENDING_RESET.store(RESET_FACTORY, Ordering::Release);
                Some((ExecuteCommandResponse::STATUS_SUCCESS, b"factory_reset"))
            }
            ExecuteCommandRequest::COMMAND_BEGIN_SOFTWARE_UPDATE => {
                if request.parameter.is_empty()
                    || request.parameter.len() > FilePath::MAX_LENGTH as usize
                {
                    Some((ExecuteCommandResponse::STATUS_BAD_PARAMETER, b"bad path"))
                } else if ota.active {
                    Some((ExecuteCommandResponse::STATUS_BAD_STATE, b"ota busy"))
                } else if ota.start(source_node, &request.parameter).is_err() {
                    Some((ExecuteCommandResponse::STATUS_BAD_PARAMETER, b"bad path"))
                } else {
                    let path_preview =
                        core::str::from_utf8(&request.parameter).unwrap_or("<non-utf8-path>");
                    log::info!(
                        "ota begin: server={} path_len={} path='{}'",
                        source_node.to_u8(),
                        request.parameter.len(),
                        path_preview
                    );
                    Some((ExecuteCommandResponse::STATUS_SUCCESS, b"ota"))
                }
            }
            // Unrecognised command — let the caller try a vendor-specific handler.
            _ => None,
        }
    }
}
