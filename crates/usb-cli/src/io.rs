use core::cmp::min;
use core::fmt::{Error as FmtError, Result as FmtResult, Write as FmtWrite};
use core::usize;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_usb::driver::EndpointError;
use embedded_io_async::{ErrorKind, Write as AsyncWrite};
use fixed_queue::VecDeque;

use crate::{CAP, PUBS, SUBS};

pub struct IO<'a> {
    pub stdin: embassy_sync::pubsub::Subscriber<'a, CriticalSectionRawMutex, u8, CAP, 1, PUBS>,
    queue: VecDeque<u8, 64>,
    pub stdout: embassy_sync::pubsub::Publisher<'a, CriticalSectionRawMutex, u8, CAP, SUBS, 1>,
}

impl<'a> IO<'a> {
    pub fn new(
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
