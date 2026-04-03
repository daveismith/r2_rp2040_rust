//! Unit tests for the new [`NodeExtension`] methods:
//! `on_tick`, `register_publishers`, `handle_execute_command`,
//! and `preferred_loop_period`.

extern crate alloc;

use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::CanTransport;
use canadensis_data_types::uavcan::node::execute_command_1_3::{
    ExecuteCommandRequest, ExecuteCommandResponse,
};
use embassy_time::Duration;

use cyphal_node::{
    execute_command::DefaultCommandHandler, CyphalHandler, NodeExtension, NoopHandler, OtaSession,
};

// ---- Minimal NodeExtension impl for testing -------------------------------

struct TestExtension {
    last_command: Option<u16>,
    preferred_period: Duration,
}

impl TestExtension {
    fn new() -> Self {
        Self {
            last_command: None,
            preferred_period: Duration::from_millis(5),
        }
    }
}

impl TransferHandler<CanTransport> for TestExtension {
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

impl NodeExtension for TestExtension {
    fn register_subscriptions<N>(&self, _node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        true
    }

    fn handle_execute_command(
        &mut self,
        request: &ExecuteCommandRequest,
    ) -> Option<(u8, &'static [u8])> {
        self.last_command = Some(request.command);
        if request.command == 0x0001 {
            Some((ExecuteCommandResponse::STATUS_SUCCESS, b"vendor_ok"))
        } else {
            None
        }
    }

    fn preferred_loop_period(&self) -> Duration {
        self.preferred_period
    }
}

// ---- Tests ----------------------------------------------------------------

#[test]
fn noop_handler_default_preferred_loop_period() {
    let handler = NoopHandler;
    assert_eq!(handler.preferred_loop_period(), Duration::from_millis(20));
}

#[test]
fn noop_handler_handle_execute_command_returns_none() {
    let mut handler = NoopHandler;
    let request = ExecuteCommandRequest {
        command: 0x0001,
        parameter: heapless::Vec::new(),
    };
    assert_eq!(handler.handle_execute_command(&request), None);
}

#[test]
fn test_extension_preferred_loop_period_override() {
    let ext = TestExtension::new();
    assert_eq!(ext.preferred_loop_period(), Duration::from_millis(5));
}

#[test]
fn test_extension_handle_vendor_command_success() {
    let mut ext = TestExtension::new();
    let request = ExecuteCommandRequest {
        command: 0x0001,
        parameter: heapless::Vec::new(),
    };
    let result = ext.handle_execute_command(&request);
    assert_eq!(
        result,
        Some((ExecuteCommandResponse::STATUS_SUCCESS, b"vendor_ok" as &[u8]))
    );
    assert_eq!(ext.last_command, Some(0x0001));
}

#[test]
fn test_extension_handle_unknown_vendor_command_returns_none() {
    let mut ext = TestExtension::new();
    let request = ExecuteCommandRequest {
        command: 0x0002,
        parameter: heapless::Vec::new(),
    };
    let result = ext.handle_execute_command(&request);
    assert_eq!(result, None);
    assert_eq!(ext.last_command, Some(0x0002));
}

#[test]
fn cyphal_handler_delegates_preferred_loop_period() {
    let cmd = DefaultCommandHandler::new(None, 0x480000..0x500000);
    let handler = CyphalHandler::new(cmd, TestExtension::new());
    assert_eq!(handler.preferred_loop_period(), Duration::from_millis(5));
}

#[test]
fn cyphal_handler_noop_extension_has_20ms_period() {
    let cmd = DefaultCommandHandler::new(None, 0x480000..0x500000);
    let handler = CyphalHandler::new(cmd, NoopHandler);
    assert_eq!(handler.preferred_loop_period(), Duration::from_millis(20));
}

#[test]
fn cyphal_handler_delegates_handle_execute_command() {
    let cmd = DefaultCommandHandler::new(None, 0x480000..0x500000);
    let mut handler = CyphalHandler::new(cmd, TestExtension::new());
    let request = ExecuteCommandRequest {
        command: 0x0001,
        parameter: heapless::Vec::new(),
    };
    let result = handler.handle_execute_command(&request);
    assert_eq!(
        result,
        Some((ExecuteCommandResponse::STATUS_SUCCESS, b"vendor_ok" as &[u8]))
    );
    assert_eq!(handler.extension.last_command, Some(0x0001));
}

#[test]
fn cyphal_handler_noop_handle_execute_command_returns_none() {
    let cmd = DefaultCommandHandler::new(None, 0x480000..0x500000);
    let mut handler = CyphalHandler::new(cmd, NoopHandler);
    let request = ExecuteCommandRequest {
        command: 0x0001,
        parameter: heapless::Vec::new(),
    };
    assert_eq!(handler.handle_execute_command(&request), None);
}
