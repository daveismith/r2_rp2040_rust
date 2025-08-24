use core::str::FromStr;
use core::error::Error;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSettingsError {
}

impl fmt::Display for ParseSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[allow(deprecated)]
        self.description().fmt(f)
    }
}

impl Error for ParseSettingsError {

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settings {
    CanId,
    CanReportInterval
}

impl FromStr for Settings {

    type Err = ParseSettingsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().trim() {
            "canid" => Ok(Settings::CanId),
            "id" => Ok(Settings::CanId),
            "canreportinterval" => Ok(Settings::CanReportInterval),
            "reportinterval" => Ok(Settings::CanReportInterval),
            _ => Err(ParseSettingsError {  })
        }
    }

}

impl sequential_storage::map::Key for Settings {
    fn serialize_into(&self, buffer: &mut [u8]) -> Result<usize, sequential_storage::map::SerializationError> {
        let val = (*self) as usize;
        let len = size_of::<usize>();
        if buffer.len() < len {
            return Err(sequential_storage::map::SerializationError::BufferTooSmall);
        }
        buffer[..len].copy_from_slice(&val.to_le_bytes());
        Ok(len)
    }

    fn deserialize_from(buffer: &[u8]) -> Result<(Settings, usize), sequential_storage::map::SerializationError> {
        let len = size_of::<usize>();
        if buffer.len() < len {
            return Err(sequential_storage::map::SerializationError::BufferTooSmall);
        }

        let val = usize::from_le_bytes(buffer[..len].try_into().unwrap());
        let ret = match val {
            0 => Settings::CanId,
            1 => Settings::CanReportInterval,
            _ => panic!("Invalid usize for Settings")
        };
        Ok((
            ret,
            size_of::<usize>(),
        ))
    }

    fn get_len(_buffer: &[u8]) -> Result<usize, sequential_storage::map::SerializationError> {
        Ok(size_of::<usize>())
    }
}