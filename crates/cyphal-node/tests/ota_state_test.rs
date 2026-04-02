//! Unit tests for [`OtaSession`] state machine.
//!
//! All tests use [`OtaSession::start_at_ticks`] to avoid needing a live
//! Embassy time driver (which would require hardware or a mock driver setup).

use canadensis_can::CanNodeId;
use cyphal_node::OtaSession;

fn min_node() -> CanNodeId {
    CanNodeId::MIN
}

#[test]
fn new_session_is_inactive() {
    let s = OtaSession::new();
    assert!(!s.active);
    assert_eq!(s.next_offset, 0);
    assert_eq!(s.requests_sent, 0);
    assert_eq!(s.responses_received, 0);
}

#[test]
fn start_valid_path_activates() {
    let mut s = OtaSession::new();
    let result = s.start_at_ticks(min_node(), b"/fw/app.bin", 0);
    assert!(result.is_ok());
    assert!(s.active);
    assert_eq!(s.next_offset, 0);
    assert_eq!(s.requests_sent, 0);
}

#[test]
fn start_empty_path_rejected() {
    let mut s = OtaSession::new();
    assert!(s.start_at_ticks(min_node(), b"", 0).is_err());
    assert!(!s.active, "session must remain inactive after rejected start");
}

#[test]
fn start_oversized_path_rejected() {
    let mut s = OtaSession::new();
    // FilePath::MAX_LENGTH is 255; build a path of 256 bytes.
    let long_path = [b'x'; 256];
    assert!(s.start_at_ticks(min_node(), &long_path, 0).is_err());
    assert!(!s.active);
}

#[test]
fn start_when_already_active_rejected() {
    let mut s = OtaSession::new();
    s.start_at_ticks(min_node(), b"/fw/a.bin", 0).unwrap();
    assert!(s.active);
    // Second start must fail.
    let result = s.start_at_ticks(min_node(), b"/fw/b.bin", 0);
    assert!(result.is_err(), "start must fail when already active");
}

#[test]
fn clear_resets_all_state() {
    let mut s = OtaSession::new();
    s.start_at_ticks(min_node(), b"/fw/app.bin", 100).unwrap();
    assert!(s.active);

    s.clear();

    assert!(!s.active);
    assert_eq!(s.next_offset, 0);
    assert_eq!(s.requests_sent, 0);
    assert_eq!(s.responses_received, 0);
    assert_eq!(s.chunks_written, 0);
    assert!(!s.waiting_response);
    assert_eq!(s.response_timeouts, 0);
    assert!(s.pending_response_payload.is_none());
}

#[test]
fn start_after_clear_succeeds() {
    let mut s = OtaSession::new();
    s.start_at_ticks(min_node(), b"/fw/a.bin", 0).unwrap();
    s.clear();
    // Should be able to start a new session after clearing.
    assert!(s.start_at_ticks(min_node(), b"/fw/b.bin", 0).is_ok());
    assert!(s.active);
}

#[test]
fn max_length_path_accepted() {
    let mut s = OtaSession::new();
    // FilePath::MAX_LENGTH == 255, so exactly 255 bytes should be accepted.
    let exact_path = [b'a'; 255];
    assert!(s.start_at_ticks(min_node(), &exact_path, 0).is_ok());
    assert!(s.active);
}
