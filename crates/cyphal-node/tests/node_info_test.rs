//! Unit tests for [`NodeInfoConfig`] builder.

use cyphal_node::NodeInfoConfig;

#[test]
fn builder_explicit_fields() {
    let uid = [0x01u8; 16];
    let info = NodeInfoConfig::new(uid)
        .with_name("test-device")
        .with_software_version(2, 5)
        .with_hardware_version(3, 1)
        .with_vcs_revision_id(0xabcdef)
        .build_with_env("fallback", "0", "0");

    let name = core::str::from_utf8(&info.name).expect("name should be valid UTF-8");
    assert_eq!(name, "test-device");
    assert_eq!(info.software_version.major, 2);
    assert_eq!(info.software_version.minor, 5);
    assert_eq!(info.hardware_version.major, 3);
    assert_eq!(info.hardware_version.minor, 1);
    assert_eq!(info.software_vcs_revision_id, 0xabcdef);
    assert_eq!(info.unique_id, uid);
    assert_eq!(info.protocol_version.major, 1);
    assert_eq!(info.protocol_version.minor, 0);
}

#[test]
fn builder_defaults_from_env() {
    let uid = [0x02u8; 16];
    let info = NodeInfoConfig::new(uid).build_with_env("my-crate", "1", "2");

    let name = core::str::from_utf8(&info.name).expect("name should be valid UTF-8");
    assert_eq!(name, "my-crate");
    assert_eq!(info.software_version.major, 1);
    assert_eq!(info.software_version.minor, 2);
    // hardware version defaults to 1.0
    assert_eq!(info.hardware_version.major, 1);
    assert_eq!(info.hardware_version.minor, 0);
    // vcs id defaults to 0
    assert_eq!(info.software_vcs_revision_id, 0);
}

#[test]
fn builder_version_str_non_numeric() {
    let uid = [0u8; 16];
    let info = NodeInfoConfig::new(uid)
        .with_software_version_str("not_a_number", "also_bad")
        .build_with_env("x", "0", "0");
    // Non-numeric strings should silently default to 0.
    assert_eq!(info.software_version.major, 0);
    assert_eq!(info.software_version.minor, 0);
}

#[test]
fn builder_explicit_overrides_env_defaults() {
    let uid = [0x03u8; 16];
    let info = NodeInfoConfig::new(uid)
        .with_software_version(7, 8)
        .build_with_env("fallback-name", "1", "2");

    // Explicit version takes priority over env defaults.
    assert_eq!(info.software_version.major, 7);
    assert_eq!(info.software_version.minor, 8);
    // Name falls back to env since it wasn't set explicitly.
    let name = core::str::from_utf8(&info.name).expect("name should be valid UTF-8");
    assert_eq!(name, "fallback-name");
}

#[test]
fn builder_unique_id_preserved() {
    let uid = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
    ];
    let info = NodeInfoConfig::new(uid).build();
    assert_eq!(info.unique_id, uid);
}
