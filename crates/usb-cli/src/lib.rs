#![no_std]
pub mod handlers;
pub mod io;

extern crate alloc;
use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Timer;
use heapless::Vec;
use noline::builder::EditorBuilder;

pub const CAP: usize = 128;
pub const SUBS: usize = 1;
pub const PUBS: usize = 1;
pub const MAX_ARGS: usize = 8;
const MAX_LINE_SIZE: usize = 64;

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

// CLI Handler
pub async fn cli_handler(
    subscriber: embassy_sync::pubsub::Subscriber<'static, CriticalSectionRawMutex, u8, CAP, 1, PUBS>,
    publisher: embassy_sync::pubsub::Publisher<'static, CriticalSectionRawMutex, u8, CAP, SUBS, 1>,
    commands: &[Command<io::IO<'_>>],
    prompt: &'static str
) {
    let mut io = io::IO::new(subscriber, publisher);
    let mut buffer = [0; MAX_LINE_SIZE];
    let mut history = [0; MAX_LINE_SIZE];

    let dispatcher = CommandDispatcher::new(commands);

    loop {
        let mut editor = EditorBuilder::from_slice(&mut buffer)
            .with_slice_history(&mut history)
            .build_async(&mut io)
            .await
            .unwrap();

        while let Ok(line) = editor.readline(prompt, &mut io).await {
            //writeln!(io, "my read: '{}'", line).ok();
            dispatcher.dispatch(&line, &mut io).await;
        }
    }
}