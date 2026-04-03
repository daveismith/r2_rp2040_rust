# Shoulder Sensor

The shoulder sensor application interfaces with a TLV493D 3-axis magnetic field
sensor to measure joint angle and publishes the result over Cyphal/CAN.

## Hardware

| Component | Description |
|-----------|-------------|
| RP2040 Feather | Main microcontroller |
| [Adafruit TLV493D Triple-Axis Magnetometer](https://www.adafruit.com/product/5724) | Provides 3-axis magnetic field for angle calculation, connected via I2C (STEMMA QT) |
| MCP25xx CAN transceiver | Cyphal/CAN physical layer (SPI1) |

## Building

```bash
cargo build --release -p shoulder-sensor --target thumbv6m-none-eabi
```

## Cyphal Node Behaviour

### PnP Node-ID Allocation

The node uses Cyphal Plug-and-Play v1 to obtain a dynamic node ID at startup.
Configure your PnP allocator to assign the desired node ID.  All joints
running this firmware are distinguished by their **source node ID** on the bus.

### Published Subjects

| Type | Default Subject ID | Rate | Unit | Description |
|------|--------------------|------|------|-------------|
| `uavcan.si.unit.angle.Scalar` v1.0 | **6144** | 100 Hz | radians, [−π, +π] | Magnetic joint angle, zero-offset corrected |
| `uavcan.si.unit.temperature.Scalar` v1.0 | **6145** | 1 Hz | kelvin | Sensor die temperature |

Subject IDs are in the vendor-specific range (6144–7167) and can be changed
per-joint via the USB CLI (see below).

### Vendor ExecuteCommand

| Command code | Name | Effect |
|---|---|---|
| `0x0001` | `COMMAND_ZERO` | Sets the current measured angle as the zero reference.  Stored to NVS; survives reboot. |

Invoke with yakut:
```bash
y call <node_id> uavcan.node.ExecuteCommand.1.3 '{command: 1}'
```

## USB CLI Commands

Connect to the USB CDC-ACM serial port at any baud rate.

| Command | Description |
|---------|-------------|
| `angle` | Print the current angle in radians |
| `temp` | Print the current temperature in °C |
| `uptime` | Print the uptime in seconds |
| `zero` | Set the current angle as the zero reference (also persists to NVS) |
| `subject` | Show current subject IDs and zero offset |
| `subject angle <id>` | Set the angle subject ID (range 6144–7167, persists to NVS) |
| `subject temp <id>` | Set the temperature subject ID (range 6144–7167, persists to NVS) |
| `version` | Print firmware version and git hash |
| `restart` | Soft-reboot the device |
| `bootload` | Enter USB bootloader (UF2 drag-and-drop) |

## Multi-Instance Deployment

When multiple joints each run this firmware, the recommended approach is:
- Keep the default subject IDs (6144 / 6145) the same on all nodes
- Demultiplex by **source node ID** at the receiver; each joint has a unique
  node ID assigned by the PnP allocator
- Only override subject IDs if your network topology requires it (e.g. to
  route specific joints to different subscribers without node-ID filtering)

## Zero-Offset Calibration

The zero offset (`f32` radians) is subtracted from the raw angle before
publishing.  The result is wrapped to [−π, +π] so it stays physically
meaningful.

To calibrate:
1. Position the joint at the desired zero (neutral) position.
2. Run `zero` over USB CLI, or send `COMMAND_ZERO` via Cyphal.

The offset is stored in NVS and persists across reboots.

