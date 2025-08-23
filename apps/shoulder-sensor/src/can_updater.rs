use crate::can::NEEDS_TICK_SIGNAL;
use crate::can_consumer::CanFrameConsumer;
use crate::isotp::{IsoTpMessage, IsoTpNode, MAX_PAYLOAD_SIZE};
use crate::{built_info, FlashMutex, FlashType};

extern crate alloc;
use alloc::boxed::Box;
use alloc::fmt;
use async_trait::async_trait;
use embassy_boot_rp::{AlignedBuffer, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_embedded_hal::flash::partition::Partition;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};
use embassy_sync::channel::{Channel, Receiver};

use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use embedded_can::ExtendedId;
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use slice_copy::copy;

type FlashPartition<'a> = Partition<'a, CriticalSectionRawMutex, FlashType>;

const RX_CHANNEL_CAPACITY: usize = 2;
static DFU_SIGNAL: Channel<
    CriticalSectionRawMutex,
    Vec<u8, MAX_PAYLOAD_SIZE>,
    RX_CHANNEL_CAPACITY,
> = Channel::new();

const TX_MAX_SIZE: usize = 100;
const TX_CHANNEL_CAPACITY: usize = 2;
type TxChannelReceiver<'a> =
    Receiver<'a, CriticalSectionRawMutex, Vec<u8, TX_MAX_SIZE>, TX_CHANNEL_CAPACITY>;
static TX_CHANNEL: Channel<CriticalSectionRawMutex, Vec<u8, TX_MAX_SIZE>, TX_CHANNEL_CAPACITY> =
    Channel::new();

#[derive(Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum CanUpdaterCommandEnum {
    GetAttribute = 0,
    GetAttributeResponse = 1,
    SetAttribute = 2,
    SetAttributeResponse = 3,
    ImageBlock = 4,
    ImageBlockResponse = 5,
    ApplyImage = 6,
}

#[derive(Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
enum CanUpdaterAttribute {
    ActiveFileVersion = 0, // a string
    FileOffset = 1,        // a u32
    FileSize = 2,          // a u32
}

#[derive(Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum CanUpdaterStatusEnum {
    Success = 0,
    Failure = 1,
    UnsupportedAttribute = 0x86,
}

pub fn from_linkerfile<'a>(
    dfu_flash: &'a FlashMutex,
    state_flash: &'a FlashMutex,
) -> FirmwareUpdaterConfig<FlashPartition<'a>, FlashPartition<'a>> {
    extern "C" {
        static __bootloader_state_start: u32;
        static __bootloader_state_end: u32;
        static __bootloader_dfu_start: u32;
        static __bootloader_dfu_end: u32;
    }

    let dfu = unsafe {
        let start = &__bootloader_dfu_start as *const u32 as u32;
        let end = &__bootloader_dfu_end as *const u32 as u32;
        Partition::new(dfu_flash, start, end - start)
    };
    let state = unsafe {
        let start = &__bootloader_state_start as *const u32 as u32;
        let end = &__bootloader_state_end as *const u32 as u32;
        Partition::new(state_flash, start, end - start)
    };

    FirmwareUpdaterConfig { dfu, state }
}

/// A handler that drives ISO-TP on a single tx/rx ID pair.
pub struct CanFirmwareUpdater<'a, M: RawMutex + Sync, C: embedded_can::blocking::Can> {
    node: IsoTpNode<'a, M, C>,
    node_id: u32,
    base_cmd_id: u8,
    tx_receiver: TxChannelReceiver<'a>,
}

impl<'a, M: RawMutex + Sync, C: embedded_can::blocking::Can> CanFirmwareUpdater<'a, M, C> {
    /// `can` is your shared CAN interface.
    /// `node_id` is the node id the device listens on (e.g. 5).
    /// `base_cmd_id` is the base command id you listen on, and you respond on +1 (e.g. 2 (and 3)).
    pub fn new(can: &'a Mutex<M, C>, node_id: u32, base_cmd_id: u8) -> Self {
        let rx_id = embedded_can::Id::Extended(
            ExtendedId::new((node_id << 5) + (base_cmd_id & 0x1f) as u32).unwrap(),
        );
        let tx_id = embedded_can::Id::Extended(
            ExtendedId::new((node_id << 5) + (base_cmd_id & 0x1f) as u32 + 1).unwrap(),
        );

        let node = IsoTpNode::new(can, tx_id, rx_id);
        let tx_receiver = TX_CHANNEL.receiver();
        Self {
            node,
            node_id,
            base_cmd_id,
            tx_receiver,
        }
    }

    fn update_node(&mut self) {
        let rx_id = embedded_can::Id::Extended(
            ExtendedId::new((self.node_id << 5) + (self.base_cmd_id & 0x1f) as u32).unwrap(),
        );
        let tx_id = embedded_can::Id::Extended(
            ExtendedId::new((self.node_id << 5) + (self.base_cmd_id & 0x1f) as u32 + 1).unwrap(),
        );

        self.node.set_rx_id(rx_id);
        self.node.set_tx_id(tx_id);
    }
}

#[async_trait]
impl<'a, M, C, T> CanFrameConsumer<T> for CanFirmwareUpdater<'a, M, C>
where
    M: RawMutex + Sync + Send + 'static,
    C: embedded_can::blocking::Can + Send + 'static,
    T: embedded_can::Frame + Sync + Send + fmt::Debug + 'static,
{
    fn set_node_id(&mut self, node_id: u32) {
        self.node_id = node_id;
        self.update_node();
    }

    /// Accept _all_ non-remote frames (dispatcher already filtered by node_id)
    fn accepts(&self, cmd_id: u8, _is_remote: bool) -> bool {
        cmd_id == self.base_cmd_id
    }

    async fn on_frame(&mut self, frame: &T) {
        // drive ISO-TP receive state machine
        if let Ok(Some(IsoTpMessage::Complete(payload))) = self.node.receive(frame).await {
            DFU_SIGNAL.try_send(payload).unwrap();
        }
    }

    async fn tick(&mut self) {
        // Tick The Function
        let mut val = [0 as u8; TX_MAX_SIZE];

        let len = {
            let value = self.tx_receiver.receive().await;
            copy(&mut val, &value)
        };
        match self.node.send(&val[0..len]).await {
            Ok(_) => {}
            Err(_) => {
                log::error!("Failed to send payload");
            }
        };
    }
}

pub struct CanFirmwareUpdaterState {
    file_offset: usize,
    file_size: usize,
}

impl CanFirmwareUpdaterState {
    pub fn new() -> Self {
        Self {
            file_offset: 0,
            file_size: 0,
        }
    }

    pub fn update(&mut self, offset: usize, size: usize) {
        self.file_offset = offset;
        self.file_size = size;
    }
}

/// Processes an attribute get command over the CAN interface
///
/// 0                   1
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |          attr_id(2)           |
/// +-------------------------------+
///
/// The attribute id is transmitted as a big endian value.
///
/// This will be used to look up the attribute id and send back
/// a response. The format of the frame is.
///
/// 0                   1                   2                   3
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+  
/// |            attr_id(2)         |   status(1)   | val(variable) |
/// +-------------------------------+---------------+---------------+
/// |                              ...                              |
/// +---------------------------------------------------------------+
///
/// The response consists of the attribute id followed by the status.
/// If the status is SUCCESS then the value will be transmitted in
/// a big endian first format.
async fn process_attribute_get(payload: &[u8], state: &CanFirmwareUpdaterState) {
    if payload.len() < 2 {
        return;
    }

    // Start Building The Response
    let mut v = Vec::<u8, TX_MAX_SIZE>::new();
    v.push(CanUpdaterCommandEnum::GetAttributeResponse as u8)
        .unwrap();

    let attribute_id = u16::from_be_bytes(*payload.first_chunk::<2>().unwrap());
    v.extend(attribute_id.to_be_bytes());

    let attribute = match CanUpdaterAttribute::try_from(attribute_id) {
        Ok(attribute) => {
            v.push(CanUpdaterStatusEnum::Success.into()).unwrap();
            Some(attribute)
        }
        Err(_) => {
            v.push(CanUpdaterStatusEnum::UnsupportedAttribute.into())
                .unwrap();
            None
        }
    };

    match attribute {
        Some(CanUpdaterAttribute::ActiveFileVersion) => {
            v.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes())
                .unwrap();
            v.extend_from_slice("-".as_bytes()).unwrap();
            v.extend_from_slice(built_info::GIT_COMMIT_HASH_SHORT.unwrap().as_bytes())
                .unwrap();
        }
        Some(CanUpdaterAttribute::FileOffset) => v.extend(state.file_offset.to_be_bytes()),
        Some(CanUpdaterAttribute::FileSize) => v.extend(state.file_size.to_be_bytes()),
        None => log::info!("Invalid Attribute: {:?}", attribute_id),
    };

    TX_CHANNEL.try_send(v).unwrap();
    NEEDS_TICK_SIGNAL.signal(true);
}

/// Processes an attribute get command over the CAN interface
///
/// 0                   1                   2                   3
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+  
/// |            attr_id(2)         |   status(1)   | val(variable) |
/// +-------------------------------+---------------+---------------+
/// |                              ...                              |
/// +---------------------------------------------------------------+
///
/// 0                   1                   2
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |            attr_id(2)         |   status(1)   |
/// +-----------------------------------------------+
async fn process_attribute_set(_payload: &[u8]) {
    log::info!("process attribute set");
    NEEDS_TICK_SIGNAL.signal(true);
}

async fn process_image_block(
    updater: &mut FirmwareUpdater<'_, FlashPartition<'_>, FlashPartition<'_>>,
    payload: &[u8],
    state: &mut CanFirmwareUpdaterState,
) {
    if let Some((first, second)) = payload.split_first_chunk::<8>() {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&first[0..4]);
        let file_length = u32::from_be_bytes(buf) as usize;

        buf.copy_from_slice(&first[4..8]);
        let offset = u32::from_be_bytes(buf) as usize;
        let received = offset + second.len();

        state.update(received, file_length);

        if offset == 0 {
            log::info!("Firmware transfer started.");
            log::info!(" - file length: {:?}", file_length);
            log::info!(" - first block size: {:?}", second.len());
        }

        match updater.write_firmware(offset, &payload[8..]).await {
            Ok(()) => {}
            Err(err) => {
                log::error!("Bootload Error: Failed writing at offset {:?}", offset);
                log::error!(" - error: {:?}", err)
            }
        };
    }
}

async fn process_apply_image(
    updater: &mut FirmwareUpdater<'_, FlashPartition<'_>, FlashPartition<'_>>,
    _payload: &[u8], 
    state: &CanFirmwareUpdaterState
) {
    log::info!("process apply image");

    if state.file_offset >= state.file_size {
        log::info!("Firmware transfer complete.");
        //TODO: Add verification
        match updater.mark_updated().await {
            Ok(()) => {}
            Err(err) => {
                log::error!("Bootload Error: Failed to mark DFU updated");
                log::error!(" - error: {:?}", err);
            }
        }

        log::info!("Firmware update: Resetting to switch images. This may take some time");
        Timer::after_millis(100).await; // give a bit of time to pump the message out to the port
        cortex_m::peripheral::SCB::sys_reset();
    };
}

#[embassy_executor::task]
pub async fn can_updater_task(flash: &'static FlashMutex) {
    // Wait for a few seconds to allow the USB logging interface to come up.
    Timer::after_secs(2).await;

    // Create the firmware updater interface
    let config = from_linkerfile(flash, flash);
    let mut aligned = AlignedBuffer([0; 4]);
    let mut updater = FirmwareUpdater::new(config, &mut aligned.0);

    // Check the state
    let mark_boot = match updater.get_state().await {
        Ok(State::Revert) => {
            log::info!("Bootload State: Revert");
            true
        }
        Ok(state) => {
            log::info!("Bootload State: {:?}", state);
            false
        }
        Err(err) => {
            log::info!("Bootloader Error: {:?}", err);
            false
        }
    };

    if mark_boot {
        match updater.mark_booted().await {
            Ok(()) => log::info!("Marked image as booted"),
            Err(err) => log::info!("Failed to mark image as booted: {:?}", err),
        };
    }

    let mut state = CanFirmwareUpdaterState::new();

    loop {
        // Wait for a payload packet to be received via CAN.
        //let payload = DFU_SIGNAL.wait().await;
        let payload = DFU_SIGNAL.receive().await;

        // The payload format that we expect is:
        // | Bytes | Content
        // | ----- | -------
        // |    0  | Command Id
        // |  1-   | Command Payload
        match payload
            .split_first()
            .map(|x| (CanUpdaterCommandEnum::try_from(*x.0).unwrap(), x.1))
        {
            Some((CanUpdaterCommandEnum::GetAttribute, payload)) => {
                process_attribute_get(payload, &mut state).await
            }
            Some((CanUpdaterCommandEnum::SetAttribute, payload)) => {
                process_attribute_set(payload).await
            }
            Some((CanUpdaterCommandEnum::ImageBlock, payload)) => {
                process_image_block(&mut updater, payload, &mut state).await
            }
            Some((CanUpdaterCommandEnum::ApplyImage, payload)) => {
                process_apply_image(&mut updater, payload, &state).await
            }
            Some((_, _)) => {} // Just Drop Messages Which We Don't Process
            None => {}
        }
    }
}
