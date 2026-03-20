use usb_serial::{UsbPipeReader, UsbPipeWriter};
use crate::{FlashMutex, FLASH_RANGE};
use crate::cli_commands;

#[embassy_executor::task]
pub async fn cli_task(flash: &'static FlashMutex,
    tx: UsbPipeWriter<'static>,
    rx: UsbPipeReader<'static>
) {
    // Build the command registry
    let version = usb_cli::Command::new("version", "Print Version Details", cli_commands::VersionCommand);
    let echo = usb_cli::Command::new("echo", "Echo input", usb_cli::handlers::EchoCommand);
    let bootload = usb_cli::Command::new("bootload", "Launch USB Bootloader", usb_cli::handlers::BootloadCommand);
    let cpu = usb_cli::Command::new("cpu", "Check CPU Usage", usb_cli::cpu_handler::CpuCommand);
    let restart = usb_cli::Command::new("restart", "Restart the system", usb_cli::handlers::RestartCommand);
    
    //Specific 
    let uptime = usb_cli::Command::new("uptime", "Check uptime of the device", cli_commands::UptimeCommand);
    let angle = usb_cli::Command::new("angle", "Read sensor angle", cli_commands::AngleCommand);
    let temp = usb_cli::Command::new("temp", "Read sensor temperature", cli_commands::TempCommand);
    let can = usb_cli::Command::new("can", "Configure CAN Bus", canbus::can_cli_commands::CanCommand::new(flash, &FLASH_RANGE));

    // Create the dispatcher with the registry.
    let commands = &[version, echo, uptime, angle, temp, can, bootload, cpu, restart, ];

    let prompt = "> ";

    usb_cli::cli_handler(tx, rx, commands, prompt).await;
}