# RP2040 Apps for R2-D2
This repo contains a variety of apps that I've been working on for my R2-D2. These are implemented in the [rust](https://rust-lang.org/) programming language using the [embassy](https://embassy.dev/) framework for development.

## Projects
* [shoulder-sensor](./apps/shoulder-sensor/README.md) - a shoulder position sensor

## Bootloader
The bootloader is used to allow bootloading over whatever interface the application desires. To install:
1. Navigate to the `boot/embassy-bootloader` directory
2. Trigger your device into DFU mode.
3. Run `cargo run --release` to flash to the device