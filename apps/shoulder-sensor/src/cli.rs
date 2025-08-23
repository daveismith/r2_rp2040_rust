use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use embassy_time::Timer;
use heapless::Vec;

const MAX_ARGS: usize = 8;

// --- Trait for Command Handlers ---

#[async_trait(?Send)]
pub trait CommandHandler<IO>: Send + Sync {
    async fn execute(&self, args: &[&str], io: &mut IO);
}

// --- Command Struct ---

pub struct Command<IO> {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: Box<dyn CommandHandler<IO>>,
}

impl<IO> Command<IO> {
    pub fn new<H>(name: &'static str, description: &'static str, handler: H) -> Self
    where
        H: CommandHandler<IO> + 'static,
    {
        Self {
            name,
            description,
            handler: Box::new(handler),
        }
    }
}

// --- Command Dispatcher ---

pub struct CommandDispatcher<'a, IO> {
    commands: &'a [Command<IO>],
}

impl<'a, IO> CommandDispatcher<'a, IO>
where
    IO: AsyncWrite + FmtWrite,
{
    pub fn new(commands: &'a [Command<IO>]) -> Self {
        Self { commands }
    }

    pub async fn dispatch(&self, line: &str, io: &mut IO) {
        let mut args: Vec<&str, MAX_ARGS> = Vec::new();

        for arg in line.split_whitespace() {
            if args.push(arg).is_err() {
                break;
            }
        }

        if args.is_empty() {
            writeln!(io, "No command entered.").ok();
            return;
        }

        if args[0] == "help" {
            writeln!(io, "Available commands:").ok();
            for cmd in self.commands {
                writeln!(io, "  {:<10} - {}", cmd.name, cmd.description).ok();
                io.flush().await.ok();
                Timer::after_millis(10).await;
            }
            return;
        }

        for cmd in self.commands {
            if cmd.name == args[0] {
                cmd.handler.execute(&args, io).await;
                return;
            }
        }

        writeln!(io, "Unknown command: '{}'", args[0]).ok();
    }
}

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