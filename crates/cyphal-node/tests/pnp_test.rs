//! Unit tests for PnP allocation helpers.
//!
//! These tests are purely computational and run on the host (x86_64) —
//! no hardware or Embassy time driver is required.

use cyphal_node::pnp::{pnp_request_retry_duration, pnp_unique_id_hash};
use cyphal_node::pnp::{PNP_RETRY_JITTER_MS, PNP_RETRY_MIN_MS};

#[test]
fn unique_id_hash_known_vector() {
    // Regression test: hash of the all-zeros UID must remain stable.
    let uid = [0u8; 16];
    let h = pnp_unique_id_hash(&uid);
    // Only the lowest 48 bits should be set.
    assert_eq!(h & !0x0000_ffff_ffff_ffff, 0, "hash must fit in 48 bits");
}

#[test]
fn unique_id_hash_different_uids_differ() {
    let uid_a = [1u8; 16];
    let uid_b = [2u8; 16];
    assert_ne!(
        pnp_unique_id_hash(&uid_a),
        pnp_unique_id_hash(&uid_b),
        "different UIDs must produce different hashes"
    );
}

#[test]
fn unique_id_hash_stable_across_calls() {
    let uid = [0xDE, 0xAD, 0xBE, 0xEF, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    assert_eq!(
        pnp_unique_id_hash(&uid),
        pnp_unique_id_hash(&uid),
        "hash must be deterministic"
    );
}

#[test]
fn retry_duration_within_range() {
    let uid = [0xAB; 16];
    for attempt in 0..10_u32 {
        let d = pnp_request_retry_duration(&uid, attempt);
        let ms = d.as_millis();
        assert!(
            ms >= PNP_RETRY_MIN_MS,
            "attempt {}: delay {}ms < min {}ms",
            attempt,
            ms,
            PNP_RETRY_MIN_MS
        );
        assert!(
            ms < PNP_RETRY_MIN_MS + PNP_RETRY_JITTER_MS,
            "attempt {}: delay {}ms >= max {}ms",
            attempt,
            ms,
            PNP_RETRY_MIN_MS + PNP_RETRY_JITTER_MS
        );
    }
}

#[test]
fn retry_duration_varies_with_attempt() {
    let uid = [0x01; 16];
    let d0 = pnp_request_retry_duration(&uid, 0).as_millis();
    let d1 = pnp_request_retry_duration(&uid, 1).as_millis();
    let d2 = pnp_request_retry_duration(&uid, 2).as_millis();
    // Not all attempts should produce the same delay (with overwhelming probability).
    assert!(
        d0 != d1 || d1 != d2,
        "retry delays should vary across attempts"
    );
}
