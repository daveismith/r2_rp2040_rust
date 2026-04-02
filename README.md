# RP2040 Apps for R2-D2
This repo contains a variety of apps that I've been working on for my R2-D2. These are implemented in the [rust](https://rust-lang.org/) programming language using the [embassy](https://embassy.dev/) framework for development.

## Workspace crates

### Library crates (`crates/`)

| Crate | Description |
|---|---|
| [`cyphal-node`](./crates/cyphal-node/README.md) | **Reusable Cyphal/UAVCAN node library** for RP2040 + Embassy. Handles PnP node-ID allocation, GetInfo, ExecuteCommand, OTA and heartbeat. Use this as the starting point for any new Cyphal application. |
| `canbus` | Lower-level CAN framing helpers (predates `cyphal-node`; still used by `shoulder-sensor`). |
| `usb-cli` | USB serial command-line interface framework. |
| `usb-serial` | USB serial transport abstraction. |
| `config` | Shared configuration types. |

### Application firmware (`apps/`)

| App | Description |
|---|---|
| [`cyphal-template`](./apps/cyphal-template/README.md) | **Canonical example** of how to use `cyphal-node`. Demonstrates the `AppExtension` pattern for adding custom Cyphal message subscriptions alongside all the library's built-in services. |
| [`shoulder-sensor`](./apps/shoulder-sensor/README.md) | Shoulder position sensor. Uses the lower-level `canbus` crate. |

## Bootloader
The bootloader is used to allow bootloading over whatever interface the application desires. To install:
1. Navigate to the `boot/embassy-bootloader` directory
2. Trigger your device into DFU mode.
3. Run `cargo run --release` to flash to the device