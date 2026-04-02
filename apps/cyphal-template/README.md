# cyphal-template

Canonical example application for the [`cyphal-node`] library crate.

`cyphal-template` is an RP2040 firmware project that demonstrates how to build
a Cyphal/UAVCAN node using the shared [`crates/cyphal-node`](../../crates/cyphal-node)
library and the Embassy async runtime.

For a full description of the library's API, features, and integration guide
see the **[cyphal-node README](../../crates/cyphal-node/README.md)**.

## What this application adds on top of the library

| App-specific code | File | Description |
|---|---|---|
| LED colour subject | `src/can_tasks.rs` (`AppExtension`) | Subscribes to `reg.udral.physics.optics.HighColor` (subject 5999) and logs received colours |
| IDENTIFY animation | `src/identify_led.rs` | Drives a WS2812 RGB LED in a rainbow pattern while the node is being identified |
| USB CLI | `src/cli_task.rs`, `src/cli_commands.rs` | Serial debug interface over USB |
| Build metadata | `build.rs` + `built_info` module | Populates `GetInfoResponse` with git hash and package version at compile time |

## Application structure

```
src/
  main.rs          – Embassy executor setup, hardware init, spawns tasks
  can_tasks.rs     – AppExtension + can_handler Embassy task (delegates to cyphal-node)
  identify_led.rs  – WS2812 LED animation triggered by ExecuteCommand::IDENTIFY
  cli_task.rs      – USB CLI task
  cli_commands.rs  – CLI command implementations
```

## AppExtension pattern

`AppExtension` in `src/can_tasks.rs` implements
`canadensis::TransferHandler<CanTransport>` for the app-specific LED colour
subject.  It is passed as the `E` type parameter to `CyphalHandler`:

```rust
let handler = CyphalHandler::new(command_handler, AppExtension);
run_cyphal_node(spi_bus, cs, reset, int, flash, NVS_RANGE, unique_id, node_info, handler).await;
```

`CyphalHandler` dispatches the four standard Cyphal services (GetInfo,
ExecuteCommand, heartbeat, file.Read for OTA) and forwards everything else to
`AppExtension::handle_message`.

## Building

```sh
cargo build-cyphal-template
```

Requires the `thumbv6m-none-eabi` target (`rustup target add thumbv6m-none-eabi`).

## Hardware

- Adafruit RP2040 Feather (or compatible RP2040 board)
- MCP25625 / MCP2515 CAN transceiver on SPI1
- Single WS2812 RGB LED on PIN 21 (IDENTIFY animation)

## Related

- [`crates/cyphal-node`](../../crates/cyphal-node/README.md) — library documentation
- [`crates/canbus`](../../crates/canbus) — lower-level CAN framing (predates this library)
