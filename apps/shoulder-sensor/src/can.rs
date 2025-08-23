use core::ops::DerefMut;

use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_sync::blocking_mutex::raw::{RawMutex, CriticalSectionRawMutex};
use embassy_sync::mutex::Mutex;
use embassy_sync::pubsub::{PubSubChannel, Publisher};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};
use embedded_can::nb::Can;
use embedded_can::{ExtendedId, Frame};
use async_trait::async_trait;
extern crate alloc;
use alloc::boxed::Box;

use mcp25xx::bitrates::clock_16mhz::CNF_1000K_BPS;
use mcp25xx::registers::{OperationMode, CANINTE, RXB0CTRL, RXB1CTRL, RXM};
use mcp25xx::{AcceptanceFilter, CanFrame, Config, IdHeader, MCP25xx};

use embassy_futures::select::{select3, select4, Either3, Either4};

use sequential_storage::cache::NoCache;
use sequential_storage::map::fetch_item;
use static_cell::StaticCell;
use crate::util::Settings;
use crate::can_updater::CanFirmwareUpdater;

use crate::{SpiBusType, FLASH_RANGE, TLV_ANGLE, TLV_TEMP};
use crate::{FlashMutex, SpiBusMutex};
use core::sync::atomic::Ordering;

use crate::can_consumer::{CanSimpleDispatcher, CanFrameConsumer};

type CanTransciever<'a> = MCP25xx<SpiDevice<'static, CriticalSectionRawMutex, SpiBusType<'a>, Output<'static>>>;
type CanTranscieverMutex<'a> = Mutex<CriticalSectionRawMutex, CanTransciever<'a>>;

const CAP: usize = 64;
const SUBS: usize = 1;
const PUBS: usize = 2;

pub type ConfigurationEventChannelType = PubSubChannel<CriticalSectionRawMutex, ConfigurationEvent, CAP, SUBS, PUBS>;
pub type ConfigurationEventPublisherType<'a> = Publisher<'a, CriticalSectionRawMutex, ConfigurationEvent, CAP, SUBS, PUBS>;
pub static CONFIGURATION_CHANNEL: ConfigurationEventChannelType  = PubSubChannel::new();
pub static NEEDS_TICK_SIGNAL: Signal<CriticalSectionRawMutex, bool> = Signal::new();

const DEFAULT_NODE_ID: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationEvent {
    NodeIdUpdate { node_id: u32 },
    IntervalUpdate { hz: u64 },
}

fn configure_mcp25xx(
    mcp25xx: &mut CanTransciever, 
    node_id: u32
) {
    let filter_addr = ExtendedId::new(node_id << 5).unwrap(); 
    let filter_mask = ExtendedId::new(0x1FFFFFE0).unwrap();

    // Configure The Acceptance Filter To Only Accept Messages Destined For Our
    // node id
    let filters = [
        (AcceptanceFilter::Filter0, IdHeader::from(filter_addr)) ,
        (AcceptanceFilter::Filter2, IdHeader::from(filter_addr)),
        (AcceptanceFilter::Mask0, IdHeader::from(filter_mask)),
        (AcceptanceFilter::Mask1, IdHeader::from(filter_mask))
    ];

    // Configure the device into normal operations at 1000kbps
    let  config = Config::default()
        .mode(OperationMode::NormalOperation)
        .bitrate(CNF_1000K_BPS)
        .filters(&filters)
        .receive_buffer_0(RXB0CTRL::default().with_rxm(RXM::Filter))
        .receive_buffer_1(RXB1CTRL::default().with_rxm(RXM::Filter));
    mcp25xx.apply_config(&config).unwrap();

    // Configure the interrupt pin to signal when there are received messages to process
    let ints = CANINTE::default()
        .with_rx0ie(true)
        .with_rx1ie(true);
    mcp25xx.write_register(ints).unwrap();
}

fn tx_report(mcp25xx: &mut CanTransciever, node_id: u32, sequence: &mut u8) {
    // We fired because of the ticker, so we are going to send a data report.
    // Currently this consists of a sequence number, the sensor angle and the 
    // sensor temperature. Both angle & temperature are stored in a shared 
    // AtomicI16. We'll read this value and convert it to the data which we
    // then send over the line (in big endian format right now)

    let angle_var = TLV_ANGLE.load(Ordering::Relaxed);    
    let temp_var = TLV_TEMP.load(Ordering::Relaxed);
    let mut data_bytes = [angle_var.to_be_bytes(), temp_var.to_be_bytes()].concat();
    data_bytes.insert(0, *sequence);    // prepend the sequence to the start.

    *sequence = sequence.wrapping_add(1); // Increment The Counter, rolling over 

    // For now, we use a hard coded id of 123, but will soon change this to be
    // something that is either read from hardware or NVS. Then we create the
    // frame which will be sent over the wire.
    let can_id = ExtendedId::new((node_id << 5) as u32).unwrap();

    let frame = CanFrame::new(
        //Id::Extended(ExtendedId::ZERO),
        can_id,
        &data_bytes,
    );

    // If we successfully created the frame, add it to the transmit queue of the
    // CAN transceiver.
    match frame {
        None => {},
        Some(ref f) => match mcp25xx.transmit(f) {
            Ok(_) => {},
            //Err(_) => {},
            Err(error) => {
                log::info!("Tranmit Error: {:?}", error);
            }
        }
    }
}

struct MyHandler<'a, M: RawMutex, T: Clone, const CAP: usize, const SUBS: usize, const PUBS: usize> {
    configuration_publisher: embassy_sync::pubsub::Publisher<'a, M, T, CAP, SUBS, PUBS> 
}

impl<'a, M: RawMutex, T: Clone, const CAP: usize, const SUBS: usize, const PUBS: usize> MyHandler<'a, M, T, CAP, SUBS, PUBS> {

    pub const fn new(publisher: embassy_sync::pubsub::Publisher<'a, M, T, CAP, SUBS, PUBS>) -> Self {
        Self {
            configuration_publisher: publisher
        }
    }

}

#[async_trait]
impl<T: embedded_can::Frame + Sync, M: RawMutex + Sync, const CAP: usize, const SUBS: usize, const PUBS: usize> CanFrameConsumer<T> for MyHandler<'_, M, ConfigurationEvent, CAP, SUBS, PUBS> {

    fn accepts(&self, id: u8, is_remote: bool) -> bool {
        //matches!(id, Id::Standard(sid) if sid.as_raw() >= 0x600 && sid.as_raw() < 0x700)
        id == 0 && is_remote
    }

    async fn on_frame(&mut self, frame: &T) {
        // Handle the frame
        log::info!("got frame: {:?}: {:?}", frame.id(), frame.data());
        // Publish The Rate
        let message = ConfigurationEvent::IntervalUpdate { hz: 10 };
        //self.configuration_publisher.publish_immediate(message);
        self.configuration_publisher.publish(message).await;
    }

    async fn tick(&mut self) {}
}

#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<'static>,
    cs: Output<'static>,
    mut reset: Output<'static>,
    mut int: Input<'static>,
    flash: &'static FlashMutex
) {
    Timer::after_secs(2).await;
    let mut sequence = 0 as u8;

    // Read The Node ID
    let mut data_buffer: [u8; 128] = [0; 128];
    let mut node_id: u32 = {
        let mut flash = flash.lock().await;
        fetch_item::<Settings, u32, _>(
            flash.deref_mut(),
            FLASH_RANGE,
            &mut NoCache::new(),
            &mut data_buffer,
            &Settings::CanId,
        )
        .await.unwrap_or(Some(DEFAULT_NODE_ID)).unwrap_or(DEFAULT_NODE_ID)
    };

    // Reset the MCP25xx chip
    reset.set_low();
    Timer::after_millis(100).await;
    reset.set_high(); // bring the chip out of reset

    // Set up the SPI bus for connecting to the device
    let spi = SpiDevice::new(&spi_bus, cs);
    let mcp25xx  = MCP25xx { spi };
    static CAN: StaticCell<CanTranscieverMutex> = StaticCell::new();
    let can_bus = CAN.init(Mutex::new(mcp25xx));

    // Perform Initial Configuration of the MCP25xx
    {
        let mut mcp25xx = can_bus.lock().await;
        configure_mcp25xx(&mut mcp25xx, node_id);
    }

    // Set up an optional ticker variable. When present, this will trigger periodic events
    // which will cause the device to transmit the current angle.
    let mut ticker: Option<Ticker> = None;

    let mut handler = MyHandler::new(CONFIGURATION_CHANNEL.publisher().unwrap());
    let mut firmware_updater = CanFirmwareUpdater::new(can_bus, node_id, 2);

    let mut dispatcher: CanSimpleDispatcher<4, _> = CanSimpleDispatcher::new(node_id);
    dispatcher.register(&mut handler).unwrap();
    dispatcher.register(&mut firmware_updater).unwrap();

    let mut configuration_subscriber = CONFIGURATION_CHANNEL.subscriber().unwrap();

    loop {
        // Check if we need to process RX
        let (process_configuration, process_rx, process_tx, needs_tick) = match ticker {
            // We're running without a ticker, so we wait until we get an interrupt
            // indicating there is data to process. Then we indicate that we do need
            // to process a received frame (process_rx = true)
            None => {
                match select3(int.wait_for_low(), configuration_subscriber.next_message_pure(), NEEDS_TICK_SIGNAL.wait()).await {
                    Either3::First(_) => (None, true, false, false),                                // received message
                    Either3::Second(vals) => (Some(vals), false, false, false), // triggered based on settings update
                    Either3::Third(_) => (None, false, false, true),                                // indicate a tick request has been requested
                }
            },
            // We're running with a ticker, which means that there are two reasons we
            // could proceed:
            // 1. Our ticker has fired an event. We don't need to process received
            //    frames in this case (process_rx = false)
            // 2. The interrupt pin has gone low, so we indicate that we do need to
            //    process a received frame (process_rx = true)
            Some(ref mut t) => match select4(t.next(), int.wait_for_low(), configuration_subscriber.next_message_pure(), NEEDS_TICK_SIGNAL.wait()).await {
                Either4::First(_) => (None, false, true, false),                               // triggered based on the ticker
                Either4::Second(_) => (None, true, false, false),                              // triggered based on the interrupt
                Either4::Third(vals) => (Some(vals), false, false, false),  // triggered based on config
                Either4::Fourth(_) => (None, false, false, true)
            }
        };

        // Handle configuration changes
        match process_configuration {
            Some(ConfigurationEvent::NodeIdUpdate { node_id: new_id }) => {
                let mut mcp25xx = can_bus.lock().await;
                log::info!("Update Node Id to {:?}", new_id);
                node_id = new_id;
                configure_mcp25xx(&mut mcp25xx, node_id);
                dispatcher.set_node_id(new_id);
            },
            Some(ConfigurationEvent::IntervalUpdate { hz }) => { 
                log::info!("Update Interval to {:?} Hz", hz);
                ticker.replace(Ticker::every(Duration::from_hz(hz)));
            },
            None => {}
        }

        if process_rx {
            let maybe_frame = {
                let mut guard = can_bus.lock().await;
                guard.receive()
            };
            match maybe_frame {
                Ok(frame) => dispatcher.dispatch(&frame).await,
                _ => {}
            };
        }
        
        if process_tx {
            let mut mcp25xx = can_bus.lock().await;
            tx_report(&mut mcp25xx, node_id, &mut sequence);
        }

        if needs_tick {
            dispatcher.tick().await;    // Always Tick
        }
    }
}
