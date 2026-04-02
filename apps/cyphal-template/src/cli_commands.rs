use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

// Get The External Atomic Angle
use crate::built_info;
use crate::can_tasks;
use crate::UPTIME;
use core::sync::atomic::Ordering;

use usb_cli::CommandHandler;

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

pub struct NodeIdCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for NodeIdCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        if let Some(id) = can_tasks::assigned_node_id() {
            writeln!(io, "Node ID: {}", id).ok();
        } else {
            writeln!(io, "Node ID: unassigned (waiting for PnP allocation)").ok();
        }
    }
}

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