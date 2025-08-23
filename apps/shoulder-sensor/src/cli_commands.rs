use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

// Get The External Atomic Angle
use crate::built_info;
use crate::TLV_ANGLE;
use crate::TLV_TEMP;
use crate::UPTIME;
use core::sync::atomic::Ordering;

use usb_cli::CommandHandler;

pub struct AngleCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for AngleCommand
where 
    IO: AsyncWrite + FmtWrite + Send,
{

    async fn execute(&self, _args: &[&str], io: &mut IO) {
        //let shared_var = TLV_ANGLE.load(Ordering::Relaxed);
        //let data_bytes = shared_var.to_be_bytes();

        let val = TLV_ANGLE.load(Ordering::Relaxed);

        let angle = val as f64 / 100.0;
        writeln!(io, "Sensor Angle: {}", angle).ok();
    }

}

pub struct TempCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for TempCommand
where 
    IO: AsyncWrite + FmtWrite + Send,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        let val = TLV_TEMP.load(Ordering::Relaxed);
        let temp = val as f64 / 100.0;
        writeln!(io, "Sensor Temperature: {}", temp).ok();
    }   
}

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