# Shoulder Sensor

The shoulder sensor application interfaces with magnetic field sensors to detect shoulder position and movement.

## Hardware

This application uses the following Adafruit components connected via a STEMMA QT cable:

- **[Adafruit TLV493D Triple Axis Magnetometer](https://www.adafruit.com/product/5724)** - Provides 3-axis magnetic field measurements for detecting shoulder orientation and position
- **[Adafruit QT Py ESP32-S3](https://www.adafruit.com/product/4366)** - Main microcontroller board

The magnetometer communicates with the main board using the I2C protocol via the STEMMA QT connector, providing a convenient plug-and-play connection.

## Building and Running

```bash
cargo build --release
```

## Features

- Real-time magnetic field sensing
- Command-line interface for sensor monitoring and calibration

## CAN Messages

The shoulder sensor broadcasts magnetometer readings periodically via CAN bus. Messages are sent at 100 Hz using the extended CAN ID format.

### Message Format

| Field | Bytes | Type | Description |
|-------|-------|------|-------------|
| Sequence | 0 | u8 | Rolling counter (0-255) |
| Angle | 1-2 | i16 (big-endian) | Magnetic angle in 0.01° units |
| Temperature | 3-4 | i16 (big-endian) | Sensor temperature in 0.01°C units |

**Total Payload: 5 bytes**

### CAN ID

The CAN ID is calculated as: `(node_id << 5)`

