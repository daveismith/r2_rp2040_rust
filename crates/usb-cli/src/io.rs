use core::fmt::{Error as FmtError, Result as FmtResult, Write as FmtWrite};
use core::usize;

use embassy_usb::driver::EndpointError;
use embedded_io_async::{ErrorKind, Write as AsyncWrite};
use usb_serial::{UsbPipeReader, UsbPipeWriter};

pub struct IO<'a> {
    pub stdin: UsbPipeReader<'a>,
    pub stdout: UsbPipeWriter<'a>,
}

impl<'a> IO<'a> {
    pub fn new(
        stdin: UsbPipeReader<'a>,
        stdout: UsbPipeWriter<'a>,
    ) -> Self {
        Self {
            stdin,
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
        let len = self.stdin.read(buf).await;
        Ok(len)
    }
}

// Implement the noline writer trait to enable us to write to the USB output
impl<'a> AsyncWrite for IO<'a> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        //self.stdout.write_packet(buf).await?;

        match self.stdout.write_all(buf).await {
            Ok(_) => Ok(buf.len()),
            Err(_) => Err(Error(()))
        }
        //Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match self.stdout.flush().await {
            Ok(_) => Ok(()),
            Err(_) => Err(Error(()))
        }
    }
}

impl<'a> FmtWrite for IO<'a> {
    /// Writes a string slice into the tx queue, updating the length accordingly.
    fn write_str(&mut self, s: &str) -> FmtResult {
        let raw_s = s.as_bytes();
        match self.stdout.try_write(raw_s) {
            Ok(_) => Ok(()),
            Err(_) => Err(FmtError)
        }
    }
}
