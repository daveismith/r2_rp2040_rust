use usb_serial::{UsbPipeReader, UsbPipeWriter};
use crate::FlashMutex;
use crate::cli_commands;

#[embassy_executor::task]
pub async fn cli_task(
    flash: &'static FlashMutex,
    tx: UsbPipeWriter<'static>,
    rx: UsbPipeReader<'static>,
) {
    // Build the command registry
    let version = usb_cli::Command::new("version", "Print Version Details", cli_commands::VersionCommand);
    let echo    = usb_cli::Command::new("echo",    "Echo input",               usb_cli::handlers::EchoCommand);
    let bootload = usb_cli::Command::new("bootload", "Launch USB Bootloader",  usb_cli::handlers::BootloadCommand);
    let cpu     = usb_cli::Command::new("cpu",     "Check CPU Usage",          usb_cli::cpu_handler::CpuCommand);
    let restart = usb_cli::Command::new("restart", "Restart the system",       usb_cli::handlers::RestartCommand);

    // Sensor / node commands
    let uptime  = usb_cli::Command::new("uptime",  "Check uptime of the device",          cli_commands::UptimeCommand);
    let angle   = usb_cli::Command::new("angle",   "Read sensor angle (radians)",          cli_commands::AngleCommand);
    let temp    = usb_cli::Command::new("temp",    "Read sensor temperature (°C)",         cli_commands::TempCommand);
    let zero    = usb_cli::Command::new("zero",    "Set current angle as zero reference",  cli_commands::ZeroCommand { flash });
    let subject = usb_cli::Command::new(
        "subject",
        "Get/set Cyphal subject IDs  (usage: subject [angle|temp] <id>)",
        cli_commands::SubjectCommand { flash },
    );

    let commands = &[version, echo, uptime, angle, temp, zero, subject, bootload, cpu, restart];
    let prompt = "> ";
    usb_cli::cli_handler(tx, rx, commands, prompt).await;
}
