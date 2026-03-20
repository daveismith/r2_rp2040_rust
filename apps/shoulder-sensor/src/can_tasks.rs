use crate::{FlashMutex, TLV_ANGLE, TLV_TEMP};

use canbus::{SpiBusMutex, CAN_NODE_ID};
use canbus::can_updater::CanFirmwareUpdater;
use core::ops::{DerefMut, Range};
use core::sync::atomic::Ordering;
use embedded_can::{ExtendedId, Frame};
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Ticker};
use mcp25xx::{CanFrame, MCP25xx};
use sequential_storage::cache::NoCache;
use sequential_storage::map::fetch_item;
use static_cell::StaticCell;

const DEFAULT_NODE_ID: u32 = 0;
static TX_QUEUE: embassy_sync::channel::Channel<CriticalSectionRawMutex, mcp25xx::CanFrame, 4> = embassy_sync::channel::Channel::new();

#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<'static, peripherals::SPI1>,
    cs: Output<'static>,
    reset: Output<'static>,
    int: Input<'static>,
    flash: &'static FlashMutex,
    flash_range: Range<u32>
) {
    // Read The Node ID
    let mut data_buffer: [u8; 128] = [0; 128];
    let node_id: u32 = {
        let mut flash = flash.lock().await;

        fetch_item::<canbus::util::Settings, u32, _>(
            flash.deref_mut(),
            flash_range,
            &mut NoCache::new(),
            &mut data_buffer,
            &canbus::util::Settings::CanId,
        )
        .await.unwrap_or(Some(DEFAULT_NODE_ID)).unwrap_or(DEFAULT_NODE_ID)
    };
    
    let fw_updater  = canbus::can_updater::CanFirmwareUpdater::new(TX_QUEUE.dyn_sender(), node_id, 2);
    static FW_HANDLER: StaticCell<CanFirmwareUpdater<'_, CanFrame>> = StaticCell::new();
    let my_fw_handler = FW_HANDLER.init(fw_updater);
    
    // Set up the SPI bus for connecting to the device
    let spi = SpiDevice::new(spi_bus, cs);
    let mcp25xx  = MCP25xx { spi };
    let can_bus = Mutex::new(mcp25xx);

    let mut can: canbus::can::CanService<'_, 4, _, mcp25xx::CanFrame> = canbus::can::CanService::new(can_bus, reset, int, node_id, TX_QUEUE.dyn_receiver());

    // Register The Handlers
    //can.register(my_handler).unwrap();

    can.register(my_fw_handler).unwrap();
    
    can.run().await

}

#[embassy_executor::task]
pub async fn can_reporter() {
    let sender: embassy_sync::channel::DynamicSender<'_, CanFrame> = TX_QUEUE.dyn_sender();
    let mut ticker = Ticker::every(Duration::from_hz(100));

    let mut sequence: u8 = 0;
    
    loop {
        ticker.next().await;

        // Grab The Data & Build The Frame
        let angle_var = TLV_ANGLE.load(Ordering::Relaxed);    
        let temp_var = TLV_TEMP.load(Ordering::Relaxed);
        let mut data_bytes = [angle_var.to_be_bytes(), temp_var.to_be_bytes()].concat();
        data_bytes.insert(0, sequence);    // prepend the sequence to the start.

        sequence = sequence.wrapping_add(1); // Increment The Counter, rolling over 

        // For now, we use a hard coded id of 123, but will soon change this to be
        // something that is either read from hardware or NVS. Then we create the
        // frame which will be sent over the wire.
        let node_id = CAN_NODE_ID.load(Ordering::Relaxed);
        let can_id = ExtendedId::new((node_id << 5) as u32).unwrap();

        let frame = Frame::new(
            can_id,
            &data_bytes,
        );

        // If we successfully created the frame, add it to the transmit queue of the
        // CAN transceiver.
        sender.send(frame.unwrap()).await;        
    }
}