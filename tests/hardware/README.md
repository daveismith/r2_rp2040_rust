# Cyphal Hardware Tests

Native-Python pytest suite that talks to a live **cyphal-template** RP2040 node
over a slcan CAN adapter.  No external tools (yakut, subprocess, etc.) are needed —
all Cyphal communication is handled by [pycyphal](https://github.com/OpenCyphal/pycyphal).

---

## Lab Setup

| Item | Details |
|------|---------|
| **Board** | Adafruit Feather RP2040 |
| **Firmware** | `apps/cyphal-template` flashed via UF2 or `cargo run` |
| **Node ID** | 42 (hard-coded in `can_tasks.rs`) |
| **CAN controller** | MCP25625 on SPI1 |
| **CAN adapter** | slcan — typically `/dev/cu.usbmodem1101` on macOS |
| **Bitrate** | 1 Mbit/s |
| **Termination** | 120 Ω at both ends of the bus |
| **WS2812 LED** | Data = GPIO21, Power enable = GPIO20 |

### Quick connectivity check

```sh
# Verify the adapter receives heartbeats before running tests:
.venv/bin/python can_check.py
```

---

## Running the Tests

### Prerequisites

The Python virtual environment already contains all required packages:

```
pycyphal    1.27.x
python-can  4.x
pytest      9.x
pytest-asyncio  1.x
```

DSDL types are compiled automatically from `public_regulated_data_types/` on the
first run and cached in `.dsdl_compiled/` (gitignored).  Subsequent runs reuse
the cache and start immediately.

### Basic run (no reboots)

```sh
.venv/bin/pytest tests/hardware/ -m "not destructive"
```

### Full run (includes RESTART and FACTORY_RESET)

```sh
.venv/bin/pytest tests/hardware/

# Exclude OTA test (default-safe path)
.venv/bin/pytest tests/hardware/ -m "not ota"
```

### OTA proof test (opt-in, destructive)

The OTA test starts a local `uavcan.file.Read` server, asks the DUT to begin
software update, serves the image blocks, then verifies the DUT reboots.

```sh
.venv/bin/pytest tests/hardware/test_ota_update.py \
    --can-iface slcan:/dev/cu.usbmodem2101@115200 \
    --run-ota \
    --ota-file-path /fw/cyphal-template.bin \
    --ota-image-file target/thumbv6m-none-eabi/debug/cyphal-template.bin
```

### Non-default adapter or node

```sh
.venv/bin/pytest tests/hardware/ \
    --can-iface slcan:/dev/cu.usbmodem2101@115200 \
    --can-bitrate 1000000 \
    --dut-node-id 42 \
    --local-node-id 247
```

### Run a single test file

```sh
.venv/bin/pytest tests/hardware/test_heartbeat.py -v
.venv/bin/pytest tests/hardware/test_get_info.py -v
.venv/bin/pytest tests/hardware/test_execute_command.py -v -m "not destructive"
```

### Run a single test by name

```sh
.venv/bin/pytest tests/hardware/ -k "test_identify"
```

---

## Test Inventory

### `test_heartbeat.py`
| Test | Description |
|------|-------------|
| `test_heartbeat_received` | DUT publishes a Heartbeat within 5 s |
| `test_heartbeat_health_nominal` | Heartbeat health field is NOMINAL (0) |
| `test_heartbeat_mode_operational` | Heartbeat mode field is OPERATIONAL (0) |
| `test_two_consecutive_heartbeats` | Uptime increases between two consecutive beats |

### `test_get_info.py`
| Test | Description |
|------|-------------|
| `test_get_info_responds` | GetInfo returns a response (no timeout) |
| `test_get_info_node_name` | Node name is `"cyphal-template"` |
| `test_get_info_software_version` | Software version is `0.4` |
| `test_get_info_unique_id` | Unique-ID is 16 bytes, non-zero |
| `test_get_info_protocol_version` | Protocol version major is `1` |

### `test_execute_command.py`
| Test | Mark | Description |
|------|------|-------------|
| `test_identify_returns_success` | — | COMMAND_IDENTIFY → STATUS_SUCCESS |
| `test_identify_idempotent` | — | Two rapid IDENTIFY calls both succeed |
| `test_bad_command_returns_bad_command` | — | Command 0x1234 → STATUS_BAD_COMMAND |
| `test_bad_command_zero_returns_bad_command` | — | Command 0x0000 → STATUS_BAD_COMMAND |
| `test_begin_software_update_rejects_empty_parameter` | — | Empty OTA path → STATUS_BAD_PARAMETER |
| `test_restart_returns_success_and_node_reappears` | `destructive` | RESTART → SUCCESS, node reappears |
| `test_factory_reset_returns_success_and_node_reappears` | `destructive` | FACTORY_RESET → SUCCESS, node reappears |

### `test_ota_update.py`
| Test | Mark | Description |
|------|------|-------------|
| `test_begin_software_update_downloads_and_reboots` | `ota`, `destructive` | Full `uavcan.file.Read` OTA transfer, mark-updated, reboot verification |

---

## Architecture Notes

### DSDL Compilation

`pycyphal.application` requires `uavcan` to be importable at Python import time.
The standard `uavcan` pip package is a metadata stub only (no Python modules).

`conftest.py` compiles the DSDL source from `public_regulated_data_types/` into
`.dsdl_compiled/` using `pycyphal.dsdl.compile_all()` **at module load time**
(before any other import), then inserts `.dsdl_compiled/` into `sys.path`.

### Event Loop

All tests run in a single session-scoped asyncio event loop (`asyncio_default_fixture_loop_scope = session` in `pytest.ini`).  This allows the session-scoped pycyphal
node — which holds the open CAN file descriptor — to be shared across all tests
without closing and re-opening the adapter between tests.

### Destructive Test Ordering

RESTART and FACTORY_RESET tests are tagged `@pytest.mark.destructive`.  pytest
runs them in file order within `test_execute_command.py`, which places them last.
If both destructive tests are run, RESTART runs first (faster reboot) and
FACTORY_RESET runs last (erases NVS, then reboots).

After a factory reset the node should start up with default configuration, which
for `cyphal-template` means the same hard-coded node ID 42, so subsequent
heartbeat detection still works.
