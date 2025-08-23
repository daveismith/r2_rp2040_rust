use embassy_usb::driver::EndpointError;
use embedded_io_async::{ErrorKind, Write as AsyncWrite};
use fixed_queue::VecDeque;
use noline::builder::EditorBuilder;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use core::cmp::min;
use core::fmt::{Error as FmtError, Result as FmtResult, Write as FmtWrite};
use core::usize;

use crate::{cli, cli_commands, FlashMutex};

pub const CAP: usize = 128;
pub const SUBS: usize = 1;
pub const PUBS: usize = 1;

pub struct IO<'a> {
    pub stdin: embassy_sync::pubsub::Subscriber<'a, CriticalSectionRawMutex, u8, CAP, 1, PUBS>,
    queue: VecDeque<u8, 64>,
    pub stdout: embassy_sync::pubsub::Publisher<'a, CriticalSectionRawMutex, u8, CAP, SUBS, 1>,
}

impl<'a> IO<'a> {
    fn new(
        stdin: embassy_sync::pubsub::Subscriber<'a, CriticalSectionRawMutex, u8, CAP, 1, PUBS>,
        stdout: embassy_sync::pubsub::Publisher<'a, CriticalSectionRawMutex, u8, CAP, SUBS, 1>,
    ) -> Self {
        Self {
            stdin,
            queue: VecDeque::new(),
            stdout,
        }
    }
}

#[derive(Debug)]
pub struct Error(());

impl From<EndpointError> for Error {
    fn from(_value: EndpointError) -> Self {
        Self(())
    }
}

impl embedded_io_async::Error for Error {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

impl<'a> embedded_io_async::ErrorType for IO<'a> {
    type Error = Error;
}

// Read data from the input and make it available asynchronously
impl<'a> embedded_io_async::Read for IO<'a> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // If the queue is empty
        while self.queue.is_empty() {
            //let mut buf: [u8; 64] = [0; 64];
            // Read a maximum of 64 bytes from the ouput
            //let len = self.stdin.read_packet(&mut buf).await?;
            // This is safe because we only ever pull data when empty
            // And the queue has the same capacity as the input buffer
            //for i in buf.iter().take(len) {
            //self.queue.push_back(*i).expect("Buffer overflow");
            //}
            let val = self.stdin.next_message_pure().await;
            self.queue.push_back(val).expect("Buffer Overflow");
        }

        if let Some(v) = self.queue.pop_front() {
            buf[0] = v;
            Ok(1)
        } else {
            Err(Error(()))
        }
    }
}

// Implement the noline writer trait to enable us to write to the USB output
impl<'a> AsyncWrite for IO<'a> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        //self.stdout.write_packet(buf).await?;

        for x in buf.iter() {
            self.stdout.publish(*x).await;
        }

        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // TODO: Implement me
        Ok(())
    }
}

impl<'a> FmtWrite for IO<'a> {
    /// Writes a string slice into the tx queue, updating the length accordingly.
    fn write_str(&mut self, s: &str) -> FmtResult {
        // Return an error if it's full
        if self.stdout.free_capacity() == 0 {
            return Err(FmtError);
        }

        // Get the raw bytes && calculate the length (truncate or full length if short)
        let raw_s = s.as_bytes();
        let num = min(raw_s.len(), self.stdout.free_capacity());

        // Push into the space
        for (i, x) in raw_s.iter().enumerate() {
            if i >= num {
                break;
            }
            self.stdout.publish_immediate(*x);
        }

        // Check if there's an error
        if num < raw_s.len() {
            Err(FmtError)
        } else {
            Ok(())
        }
    }
}

const MAX_LINE_SIZE: usize = 64;

pub async fn cli_handler(
    subscriber: embassy_sync::pubsub::Subscriber<
        'static,
        CriticalSectionRawMutex,
        u8,
        CAP,
        1,
        PUBS,
    >,
    publisher: embassy_sync::pubsub::Publisher<'static, CriticalSectionRawMutex, u8, CAP, SUBS, 1>,
    flash:  &'static FlashMutex
) {
    let prompt = "> ";

    let mut io = IO::new(subscriber, publisher);
    let mut buffer = [0; MAX_LINE_SIZE];
    let mut history = [0; MAX_LINE_SIZE];

    // Build the command registry
    let version = cli::Command::new("version", "Print Version Details", cli_commands::VersionCommand);
    let echo = cli::Command::new("echo", "Echo input", cli::EchoCommand);
    let uptime = cli::Command::new("uptime", "Check uptime of the device", cli_commands::UptimeCommand);
    let angle = cli::Command::new("angle", "Read sensor angle", cli_commands::AngleCommand);
    let temp = cli::Command::new("temp", "Read sensor temperature", cli_commands::TempCommand);
    let can = cli::Command::new("can", "Configure CAN Bus", cli::CanCommand::new(flash));
    let bootload = cli::Command::new("bootload", "Launch USB Bootloader", cli::BootloadCommand);
    let restart = cli::Command::new("restart", "Restart the system", cli::RestartCommand);

    // Create the dispatcher with the registry.
    let commands: &[cli::Command<IO>] = &[version, echo, uptime, angle, temp, can, bootload, restart, ];

    let dispatcher = cli::CommandDispatcher::new(commands);

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
