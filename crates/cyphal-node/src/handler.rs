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
pub struct CyphalHandler<E: TransferHandler<CanTransport>> {
    /// OTA firmware download session state.
    pub ota: OtaSession,
    /// Handler for the standard `ExecuteCommand` service.
    pub command_handler: DefaultCommandHandler,
    /// Application-specific extension that receives unhandled transfers.
    pub extension: E,
}

impl<E: TransferHandler<CanTransport>> CyphalHandler<E> {
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

impl<E: TransferHandler<CanTransport>> TransferHandler<CanTransport> for CyphalHandler<E> {
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
                    self.command_handler.handle(&request, transfer.header.source, &mut self.ota)
                }
                Err(_) => (
                    ExecuteCommandResponse::STATUS_BAD_PARAMETER,
                    b"invalid request".as_slice(),
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

