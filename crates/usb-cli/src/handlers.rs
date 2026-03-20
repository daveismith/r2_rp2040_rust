use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;

use crate::CommandHandler;

// --- Example Commands ---

pub struct EchoCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for EchoCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(io, "Usage: echo <message>").ok();
            return;
        }

        let mut msg: heapless::String<64> = heapless::String::new();
        for (i, word) in args[1..].iter().enumerate() {
            if i > 0 {
                msg.push(' ').unwrap();
            }
            msg.push_str(word).unwrap();
        }

        writeln!(io, "content: {}", msg).ok();
    }
}

pub struct BootloadCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for BootloadCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{
    async fn execute(&self, _args: &[&str], _io: &mut IO) {
        embassy_rp::rom_data::reset_to_usb_boot(0, 0);
    }
}

pub struct RestartCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for RestartCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{
    async fn execute(&self, _args: &[&str], _io: &mut IO) {
        cortex_m::peripheral::SCB::sys_reset();
    }
}