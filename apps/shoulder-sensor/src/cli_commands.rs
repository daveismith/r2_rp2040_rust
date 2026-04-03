use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use core::ops::DerefMut;
use core::sync::atomic::Ordering;
use embedded_io_async::Write as AsyncWrite;

use crate::built_info;
use crate::settings::{self, is_valid_subject_id, ShoulderSettings};
use crate::{ANGLE_SUBJECT_ID, NVS_RANGE, TEMP_SUBJECT_ID, TLV_ANGLE, TLV_TEMP, UPTIME, ZERO_OFFSET};

use usb_cli::CommandHandler;

// ---- AngleCommand ---------------------------------------------------------

pub struct AngleCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for AngleCommand
where
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let val = f32::from_bits(TLV_ANGLE.load(Ordering::Relaxed));
        writeln!(io, "Sensor Angle: {} rad", val).ok();
    }
}

// ---- TempCommand ----------------------------------------------------------

pub struct TempCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for TempCommand
where
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let val = TLV_TEMP.load(Ordering::Relaxed);
        let temp = val as f64 / 100.0;
        writeln!(io, "Sensor Temperature: {} °C", temp).ok();
    }
}

// ---- UptimeCommand --------------------------------------------------------

pub struct UptimeCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for UptimeCommand
where
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let time = UPTIME.load(Ordering::Acquire);
        writeln!(io, "Uptime: {} seconds", time).ok();
    }
}

// ---- VersionCommand -------------------------------------------------------

pub struct VersionCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for VersionCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let git_status = match built_info::GIT_DIRTY.unwrap() {
            true => "dirty",
            false => "clean",
        };
        writeln!(
            io,
            "{} {} git: {},{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            built_info::GIT_COMMIT_HASH_SHORT.unwrap(),
            git_status
        )
        .ok();
    }
}

// ---- ZeroCommand ----------------------------------------------------------

/// Set the current angle as the zero reference.
///
/// Reads the live sensor angle, stores it as the zero offset in memory, and
/// persists it to NVS.
pub struct ZeroCommand {
    pub flash: &'static crate::FlashMutex,
}

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for ZeroCommand
where
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let current_bits = TLV_ANGLE.load(Ordering::Relaxed);
        let current_rad = f32::from_bits(current_bits);
        ZERO_OFFSET.store(current_bits, Ordering::Relaxed);

        let mut f = self.flash.lock().await;
        let result = settings::store_u32(
            f.deref_mut(),
            NVS_RANGE,
            ShoulderSettings::ZeroOffset,
            current_bits,
        )
        .await;

        match result {
            Ok(()) => writeln!(io, "Zero offset set to {} rad", current_rad).ok(),
            Err(()) => writeln!(io, "Zero offset set to {} rad (NVS write failed)", current_rad)
                .ok(),
        };
    }
}

// ---- SubjectCommand -------------------------------------------------------

/// Get or set the Cyphal subject IDs used for publishing.
///
/// Usage:
/// - `subject` — display current subject IDs and zero offset
/// - `subject angle <id>` — set the angle subject ID (6144–7167)
/// - `subject temp <id>` — set the temperature subject ID (6144–7167)
pub struct SubjectCommand {
    pub flash: &'static crate::FlashMutex,
}

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for SubjectCommand
where
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.is_empty() {
            // Show current settings.
            let angle_id = ANGLE_SUBJECT_ID.load(Ordering::Relaxed);
            let temp_id = TEMP_SUBJECT_ID.load(Ordering::Relaxed);
            let zero_rad = f32::from_bits(ZERO_OFFSET.load(Ordering::Relaxed));
            writeln!(io, "Angle subject ID : {}", angle_id).ok();
            writeln!(io, "Temp  subject ID : {}", temp_id).ok();
            writeln!(io, "Zero offset      : {} rad", zero_rad).ok();
            return;
        }

        if args.len() < 2 {
            writeln!(
                io,
                "Usage: subject [angle|temp] <id>  (valid IDs: 6144-7167)"
            )
            .ok();
            return;
        }

        let kind = args[0];
        let id: u16 = match args[1].parse() {
            Ok(v) => v,
            Err(_) => {
                writeln!(io, "Invalid subject ID: {}", args[1]).ok();
                return;
            }
        };

        if !is_valid_subject_id(id) {
            writeln!(
                io,
                "Subject ID {} is outside the allowed range 6144-7167",
                id
            )
            .ok();
            return;
        }

        match kind {
            "angle" => {
                ANGLE_SUBJECT_ID.store(id, Ordering::Relaxed);
                let mut f = self.flash.lock().await;
                let result = settings::store_u32(
                    f.deref_mut(),
                    NVS_RANGE,
                    ShoulderSettings::AngleSubjectId,
                    id as u32,
                )
                .await;
                match result {
                    Ok(()) => writeln!(io, "Angle subject ID set to {}", id).ok(),
                    Err(()) => {
                        writeln!(io, "Angle subject ID set to {} (NVS write failed)", id).ok()
                    }
                };
            }
            "temp" => {
                TEMP_SUBJECT_ID.store(id, Ordering::Relaxed);
                let mut f = self.flash.lock().await;
                let result = settings::store_u32(
                    f.deref_mut(),
                    NVS_RANGE,
                    ShoulderSettings::TempSubjectId,
                    id as u32,
                )
                .await;
                match result {
                    Ok(()) => writeln!(io, "Temp subject ID set to {}", id).ok(),
                    Err(()) => {
                        writeln!(io, "Temp subject ID set to {} (NVS write failed)", id).ok()
                    }
                };
            }
            _ => {
                writeln!(io, "Unknown kind '{}'. Use 'angle' or 'temp'.", kind).ok();
            }
        }
    }
}
