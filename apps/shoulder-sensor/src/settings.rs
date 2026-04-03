//! NVS (non-volatile storage) settings for the shoulder-sensor application.
//!
//! Settings are stored in a sequential-storage map in the NVS flash region.
//! Each variant of [`ShoulderSettings`] maps to a `u32` value:
//!
//! | Key              | Default | Description                                   |
//! |------------------|---------|-----------------------------------------------|
//! | `AngleSubjectId` | 6144    | Cyphal subject ID for angle publications      |
//! | `TempSubjectId`  | 6145    | Cyphal subject ID for temperature publications |
//! | `ZeroOffset`     | 0       | Zero-offset in radians (stored as `f32` bits) |

use core::ops::Range;

use sequential_storage::cache::NoCache;
use sequential_storage::map::{fetch_item, store_item, SerializationError};

// ---- Default values -------------------------------------------------------

pub const DEFAULT_ANGLE_SUBJECT_ID: u16 = 6144;
pub const DEFAULT_TEMP_SUBJECT_ID: u16 = 6145;
/// Zero offset default: 0.0f32 stored as its bit pattern.
pub const DEFAULT_ZERO_OFFSET_BITS: u32 = 0u32; // 0.0f32.to_bits()

// ---- Valid subject-ID range -----------------------------------------------

/// Minimum vendor-specific Cyphal subject ID (inclusive).
pub const SUBJECT_ID_MIN: u16 = 6144;
/// Maximum vendor-specific Cyphal subject ID (inclusive).
pub const SUBJECT_ID_MAX: u16 = 7167;

/// Returns `true` if `id` is within the vendor-specific subject-ID range.
pub fn is_valid_subject_id(id: u16) -> bool {
    id >= SUBJECT_ID_MIN && id <= SUBJECT_ID_MAX
}

// ---- NVS key type ---------------------------------------------------------

/// NVS key variants for shoulder-sensor settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShoulderSettings {
    AngleSubjectId = 0,
    TempSubjectId = 1,
    ZeroOffset = 2,
}

impl sequential_storage::map::Key for ShoulderSettings {
    fn serialize_into(
        &self,
        buffer: &mut [u8],
    ) -> Result<usize, SerializationError> {
        let len = core::mem::size_of::<u32>();
        if buffer.len() < len {
            return Err(SerializationError::BufferTooSmall);
        }
        let val = *self as u32;
        buffer[..len].copy_from_slice(&val.to_le_bytes());
        Ok(len)
    }

    fn deserialize_from(
        buffer: &[u8],
    ) -> Result<(ShoulderSettings, usize), SerializationError> {
        let len = core::mem::size_of::<u32>();
        if buffer.len() < len {
            return Err(SerializationError::BufferTooSmall);
        }
        let val = u32::from_le_bytes(buffer[..len].try_into().unwrap());
        let key = match val {
            0 => ShoulderSettings::AngleSubjectId,
            1 => ShoulderSettings::TempSubjectId,
            2 => ShoulderSettings::ZeroOffset,
            _ => return Err(SerializationError::InvalidData),
        };
        Ok((key, len))
    }

    fn get_len(_buffer: &[u8]) -> Result<usize, SerializationError> {
        Ok(core::mem::size_of::<u32>())
    }
}

// ---- Async NVS helpers ----------------------------------------------------

type Flash = embassy_rp::flash::Flash<
    'static,
    embassy_rp::peripherals::FLASH,
    embassy_rp::flash::Async,
    { 8 * 1024 * 1024 },
>;

/// Fetch a `u32` setting from NVS, returning the default value on any error.
pub async fn fetch_u32(
    flash: &mut Flash,
    range: Range<u32>,
    key: ShoulderSettings,
    default: u32,
) -> u32 {
    let mut buf = [0u8; 64];
    fetch_item::<ShoulderSettings, u32, _>(
        flash,
        range,
        &mut NoCache::new(),
        &mut buf,
        &key,
    )
    .await
    .unwrap_or(Some(default))
    .unwrap_or(default)
}

/// Store a `u32` setting to NVS.
pub async fn store_u32(
    flash: &mut Flash,
    range: Range<u32>,
    key: ShoulderSettings,
    value: u32,
) -> Result<(), ()> {
    let mut buf = [0u8; 64];
    store_item::<ShoulderSettings, u32, _>(
        flash,
        range,
        &mut NoCache::new(),
        &mut buf,
        &key,
        &value,
    )
    .await
    .map_err(|_| ())
}
