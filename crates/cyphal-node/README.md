# cyphal-node

Drop-in Cyphal/UAVCAN node library for RP2040 projects using [Embassy].

`cyphal-node` encapsulates the common Cyphal protocol machinery so that
application firmware only needs to implement what is unique to that device:

| Handled by the library | Implemented by the app |
|---|---|
| PnP v1 dynamic node-ID allocation | Custom message subscriptions |
| `uavcan.node.GetInfo` | Hardware-specific LED / sensor drivers |
| `uavcan.node.ExecuteCommand` (Identify, Restart, FactoryReset, BeginSoftwareUpdate) | IDENTIFY animation callback |
| OTA firmware update via `uavcan.file.Read` | Build-time metadata (`built` crate) |
| Heartbeat publishing | — |

## Feature flags

| Feature | Default | Description |
|---|---|---|
| `mcp25xx` | ✓ | MCP25625/MCP2515 CAN driver + concrete `run_cyphal_node` async function for RP2040 GPIO/SPI/Flash |
| `defmt` | — | Pass-through feature; enables `defmt` in downstream app gating |

## Quick-start example

```rust
// apps/my-app/src/can_tasks.rs

use cyphal_node::{
    CyphalHandler, DefaultCommandHandler, NodeInfoConfig, NoopHandler,
    node_task::{FlashMutex, SpiBusMutex, run_cyphal_node},
};
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;

const NVS_RANGE: core::ops::Range<u32> = 0x480000..0x500000;

#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<peripherals::SPI1>,
    cs:    Output<'static>,
    reset: Output<'static>,
    int:   Input<'static>,
    flash: &'static FlashMutex,
    unique_id: [u8; 16],
) {
    let node_info = NodeInfoConfig::new(unique_id)
        .build_with_env(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
        );

    let command_handler = DefaultCommandHandler::new(
        None,      // no IDENTIFY callback
        NVS_RANGE, // flash range erased on FACTORY_RESET
    );

    run_cyphal_node(
        spi_bus, cs, reset, int, flash,
        NVS_RANGE, unique_id, node_info,
        CyphalHandler::new(command_handler, NoopHandler),
    ).await;
}
```

Then in `main.rs`, spawn `can_handler` like any other Embassy task.

## Extending the node

### Adding custom message subscriptions

Create an `AppExtension` struct that implements
`canadensis::TransferHandler<CanTransport>`, then wrap it in `CyphalHandler`:

```rust
use canadensis::core::SubjectId;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::CanTransport;

const MY_SUBJECT: SubjectId = SubjectId::from_truncating(1234);

pub struct AppExtension;

impl TransferHandler<CanTransport> for AppExtension {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where N: canadensis::Node<Transport = CanTransport>
    {
        if transfer.header.subject == MY_SUBJECT {
            // … deserialise and act on the message …
            true
        } else {
            false
        }
    }

    fn handle_request<N>(&mut self, _: &mut N, _: ResponseToken<CanTransport>,
                         _: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>) -> bool { false }
    fn handle_response<N>(&mut self, _: &mut N,
                          _: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>) -> bool { false }
}

// Then:
let handler = CyphalHandler::new(command_handler, AppExtension);
```

### Wiring the IDENTIFY animation

`DefaultCommandHandler::new` accepts an optional `fn(Duration)` callback that
is called when an `IDENTIFY` command is received:

```rust
let command_handler = DefaultCommandHandler::new(
    Some(|duration| my_led_module::trigger_identify(duration)),
    NVS_RANGE,
);
```

## GetInfo customisation

### Builder API

```rust
let node_info = NodeInfoConfig::new(unique_id)
    .with_name("my-device")
    .with_software_version(1, 3)
    .with_hardware_version(2, 0)
    .with_vcs_revision_id(0xdeadbeef)
    .build();
```

### Using Cargo package metadata

`build_with_env` fills in name and version from `env!("CARGO_PKG_*")` macros
at compile time.  Pass the three string slices and they are parsed into the
appropriate `Version` struct fields:

```rust
let node_info = NodeInfoConfig::new(unique_id)
    .build_with_env(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
    );
```

### Combining with `built` crate VCS revision

Add `built` to your app's `[build-dependencies]` and include `built_info` in
`main.rs`:

```rust
pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}
```

Then pass the git hash to the builder:

```rust
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
```

## Substituting a different CAN driver

The `mcp25xx` feature is on by default.  To use a different CAN transceiver:

1. Disable default features: `cyphal-node = { ..., default-features = false }`
2. Implement `canadensis_can::driver::{TransmitDriver, ReceiveDriver}` for your
   hardware.
3. Wire the driver into a `CoreNode` / `BasicNode` manually (following the same
   pattern as `run_cyphal_node` in `src/node_task.rs`).

## Build metadata step-by-step

1. Add to `apps/my-app/Cargo.toml`:

   ```toml
   [build-dependencies]
   built = { version = "0.8", features = ["git2"] }
   ```

2. Create `apps/my-app/build.rs`:

   ```rust
   fn main() {
       built::write_built_file().expect("Failed to acquire build-time information");
   }
   ```

3. Add to `apps/my-app/src/main.rs`:

   ```rust
   pub mod built_info {
       include!(concat!(env!("OUT_DIR"), "/built.rs"));
   }
   ```

## Relationship with `crates/canbus`

`crates/canbus` provides lower-level CAN framing without Cyphal semantics.
`cyphal-node` operates at the full Cyphal protocol level. They coexist and
serve different abstraction layers; no changes to existing `canbus` consumers
(e.g. `shoulder-sensor`) are required.

[Embassy]: https://embassy.dev
