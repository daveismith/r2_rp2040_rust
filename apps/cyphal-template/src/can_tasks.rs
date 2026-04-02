//! Cyphal node task for the `cyphal-template` application.
//!
//! This module is intentionally thin — all common Cyphal protocol logic
//! (PnP, heartbeat, OTA, GetInfo, ExecuteCommand) lives in the
//! `cyphal-node` library crate.  Only application-specific behaviour
//! (the LED-colour subject) is defined here.

use canadensis::core::SubjectId;
use canadensis::core::time::milliseconds;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::CanTransport;
use canadensis_data_types::reg::udral::physics::optics::high_color_0_1::HighColor;
use canadensis::encoding::Deserialize;

use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;

use cyphal_node::{
    CyphalHandler, DefaultCommandHandler, NodeExtension, NodeInfoConfig,
    node_task::{FlashMutex, SpiBusMutex, run_cyphal_node},
};

use crate::built_info;
use crate::identify_led;
use crate::NVS_RANGE;

/// Cyphal subject ID for the `reg.udral.physics.optics.HighColor` message.
const LED_COLOR_SUBJECT: SubjectId = SubjectId::from_truncating(5999);

// ---- Application-specific transfer handler --------------------------------

/// Handles the optional app-specific LED colour subscription.
///
/// All other transfers are forwarded to [`cyphal_node`]'s built-in handlers
/// via [`CyphalHandler`].
pub struct AppExtension;

impl TransferHandler<CanTransport> for AppExtension {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.subject != LED_COLOR_SUBJECT {
            return false;
        }
        if let Ok(color) = HighColor::deserialize_from_bytes(&transfer.payload) {
            log::info!(
                "LED color rx r={} g={} b={}",
                color.red,
                color.green,
                color.blue
            );
            true
        } else {
            false
        }
    }

    fn handle_request<N>(
        &mut self,
        _node: &mut N,
        _token: ResponseToken<CanTransport>,
        _transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }

    fn handle_response<N>(
        &mut self,
        _node: &mut N,
        _transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }
}

// ---- NodeExtension impl --------------------------------------------------

impl NodeExtension for AppExtension {
    fn register_subscriptions<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        node.subscribe_message(LED_COLOR_SUBJECT, 2, milliseconds(1_000))
            .is_ok()
    }
}

// ---- Node-ID helper -------------------------------------------------------

/// Returns the dynamically allocated Cyphal node ID, or `None` if PnP
/// allocation has not yet completed.
pub fn assigned_node_id() -> Option<u8> {
    cyphal_node::assigned_node_id()
}

// ---- Embassy task ---------------------------------------------------------

/// Main Cyphal task.
///
/// Builds node info from the app's Cargo metadata, wires the LED-colour
/// extension handler, then delegates to [`run_cyphal_node`] for the full
/// node lifecycle (PnP allocation → BasicNode → main loop).
#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<peripherals::SPI1>,
    cs: Output<'static>,
    reset: Output<'static>,
    int: Input<'static>,
    flash: &'static FlashMutex,
    unique_id: [u8; 16],
) {
    let node_info = NodeInfoConfig::new(unique_id)
        .with_vcs_revision_id(
            built_info::GIT_COMMIT_HASH_SHORT
                .and_then(|h| u64::from_str_radix(h, 16).ok())
                .unwrap_or(0),
        )
        .build_with_env(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
        );

    let command_handler = DefaultCommandHandler::new(
        Some(|d| identify_led::trigger_identify(d)),
        NVS_RANGE,
    );

    let handler = CyphalHandler::new(command_handler, AppExtension);

    run_cyphal_node(
        spi_bus,
        cs,
        reset,
        int,
        flash,
        NVS_RANGE,
        unique_id,
        node_info,
        handler,
    )
    .await;
}
