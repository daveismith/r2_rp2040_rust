//! Main Cyphal transfer handler that wires together OTA, ExecuteCommand, and
//! application-specific extensions.
//!
//! [`CyphalHandler`] implements [`canadensis::TransferHandler`] for
//! `CanTransport` and provides:
//!
//! - **Messages**: forwarded entirely to the `E` extension handler.
//! - **Requests**: `uavcan.node.ExecuteCommand` is handled by
//!   [`DefaultCommandHandler`]; other service requests are forwarded to `E`.
//! - **Responses**: `uavcan.file.Read` responses are captured for OTA;
//!   other responses are forwarded to `E`.
//!
//! # Extending the node
//! Create an application-specific handler that implements
//! [`canadensis::TransferHandler<CanTransport>`] and pass it as the `E`
//! type parameter. It will receive all transfers not consumed by the
//! library's built-in handling.
//!
//! Use [`NoopHandler`] when no extension is needed.

use canadensis::core::time::milliseconds;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::CanTransport;
use canadensis_data_types::uavcan::file::read_1_1;
use canadensis_data_types::uavcan::node::execute_command_1_3::{
    ExecuteCommandRequest, ExecuteCommandResponse,
};
use canadensis::encoding::Deserialize;
use embassy_time::Duration;

use crate::execute_command::DefaultCommandHandler;
use crate::ota::OtaSession;

/// A no-op [`TransferHandler`] for use when no extension is needed.
pub struct NoopHandler;

impl TransferHandler<CanTransport> for NoopHandler {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        _transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }
}

/// Combined Cyphal transfer handler.
///
/// # Type parameters
/// - `E`: application-specific extension handler (use [`NoopHandler`] if not needed)
pub struct CyphalHandler<E: NodeExtension> {
    /// OTA firmware download session state.
    pub ota: OtaSession,
    /// Handler for the standard `ExecuteCommand` service.
    pub command_handler: DefaultCommandHandler,
    /// Application-specific extension that receives unhandled transfers.
    pub extension: E,
}

impl<E: NodeExtension> CyphalHandler<E> {
    /// Create a new `CyphalHandler`.
    pub fn new(command_handler: DefaultCommandHandler, extension: E) -> Self {
        Self {
            ota: OtaSession::new(),
            command_handler,
            extension,
        }
    }

    /// Return `true` if an OTA session is currently active.
    pub fn is_ota_active(&self) -> bool {
        self.ota.active
    }
}

impl<E: NodeExtension> TransferHandler<CanTransport> for CyphalHandler<E> {
    /// Forward all message transfers to the extension handler.
    fn handle_message<N>(
        &mut self,
        node: &mut N,
        transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        self.extension.handle_message(node, transfer)
    }

    /// Handle `ExecuteCommand` service requests; forward others to the extension.
    fn handle_request<N>(
        &mut self,
        node: &mut N,
        token: ResponseToken<CanTransport>,
        transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.service
            != canadensis_data_types::uavcan::node::execute_command_1_3::SERVICE
        {
            return self.extension.handle_request(node, token, transfer);
        }

        let (status, output) =
            match ExecuteCommandRequest::deserialize_from_bytes(&transfer.payload) {
                Ok(request) => {
                    // Try the standard built-in commands first.
                    let std_result = self.command_handler.handle(
                        &request,
                        transfer.header.source,
                        &mut self.ota,
                    );
                    match std_result {
                        Some(r) => r,
                        // Unknown command — delegate to vendor-specific extension.
                        None => self
                            .extension
                            .handle_execute_command(&request)
                            .unwrap_or((
                                ExecuteCommandResponse::STATUS_BAD_COMMAND,
                                b"unsupported",
                            )),
                    }
                }
                Err(_) => (
                    ExecuteCommandResponse::STATUS_BAD_PARAMETER,
                    b"invalid request" as &'static [u8],
                ),
            };

        let mut response_output = heapless::Vec::<u8, 46>::new();
        let _ = response_output.extend_from_slice(output);
        let response = ExecuteCommandResponse { status, output: response_output };

        if let Err(_err) = node.send_response(token, milliseconds(1000), &response) {
            log::warn!("can: failed to send ExecuteCommand response");
        }

        true
    }

    /// Capture `file.Read` responses for OTA; forward others to the extension.
    fn handle_response<N>(
        &mut self,
        node: &mut N,
        transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.service != read_1_1::SERVICE {
            return self.extension.handle_response(node, transfer);
        }
        if !self.ota.active || !self.ota.waiting_response {
            return false;
        }
        if transfer.header.source != self.ota.server_node {
            return false;
        }

        let mut payload =
            heapless::Vec::<u8, { crate::ota::READ_RESPONSE_PAYLOAD_MAX }>::new();
        if payload.extend_from_slice(&transfer.payload).is_err() {
            log::warn!("ota: file.read response too large");
            return true;
        }
        self.ota.pending_response_payload = Some(payload);
        true
    }
}

// ---- Extension trait ------------------------------------------------------

/// Trait for application-specific Cyphal extensions.
///
/// Combines [`TransferHandler<CanTransport>`] with lifecycle hooks that
/// [`run_cyphal_node`][crate::node_task::run_cyphal_node] calls at
/// appropriate points in the node's lifetime.
pub trait NodeExtension: TransferHandler<CanTransport> {
    /// Subscribe to any message subjects this extension handles.
    ///
    /// Called once after the node has been promoted to a named `BasicNode`.
    /// Returns `true` on success, `false` if a subscription failed (e.g.
    /// out of capacity).
    fn register_subscriptions<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>;

    /// Register any message subjects this extension wants to publish.
    ///
    /// Called once after the node has been promoted to a named `BasicNode`,
    /// immediately after [`register_subscriptions`][Self::register_subscriptions].
    /// Returns `true` on success, `false` if a `start_publishing` call failed
    /// (e.g. out of capacity).
    fn register_publishers<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        let _ = node;
        true
    }

    /// Called on every iteration of the node's main loop.
    ///
    /// Implementations should track their own rate deadlines using
    /// [`embassy_time::Instant`] and call [`canadensis::Node::publish`] when
    /// a deadline has elapsed.  **Do not block or `.await` inside this
    /// method** — it is called synchronously from the async main loop.
    ///
    /// Rate control is entirely the extension's responsibility; the library
    /// merely guarantees that `on_tick` is called at least as frequently as
    /// [`preferred_loop_period`][Self::preferred_loop_period].
    fn on_tick<N>(&mut self, node: &mut N)
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        let _ = node;
    }

    /// Handle a vendor-specific `ExecuteCommand` request.
    ///
    /// Called when an `ExecuteCommand` arrives with a command code that the
    /// built-in [`DefaultCommandHandler`][crate::execute_command::DefaultCommandHandler]
    /// did not recognise (i.e. vendor-specific commands in the `0x0000–0x7FFF`
    /// range).
    ///
    /// Return `Some((status, output))` to send a custom response, or `None`
    /// to let the library respond with `STATUS_BAD_COMMAND`.
    fn handle_execute_command(
        &mut self,
        _request: &ExecuteCommandRequest,
    ) -> Option<(u8, &'static [u8])> {
        None
    }

    /// The preferred maximum period between main-loop iterations, used to
    /// control the CAN interrupt-wait timeout.
    ///
    /// Override this to request a faster loop rate when the extension
    /// publishes time-sensitive messages.  The default is 20 ms (50 Hz).
    /// The library may not guarantee the requested period under heavy CAN
    /// load.
    fn preferred_loop_period(&self) -> Duration {
        Duration::from_millis(20)
    }
}

impl NodeExtension for NoopHandler {
    fn register_subscriptions<N>(&self, _node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        true
    }
}

impl<E: NodeExtension> NodeExtension for CyphalHandler<E> {
    fn register_subscriptions<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        self.extension.register_subscriptions(node)
    }

    fn register_publishers<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        self.extension.register_publishers(node)
    }

    fn on_tick<N>(&mut self, node: &mut N)
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        self.extension.on_tick(node);
    }

    fn handle_execute_command(
        &mut self,
        request: &ExecuteCommandRequest,
    ) -> Option<(u8, &'static [u8])> {
        self.extension.handle_execute_command(request)
    }

    fn preferred_loop_period(&self) -> Duration {
        self.extension.preferred_loop_period()
    }
}

