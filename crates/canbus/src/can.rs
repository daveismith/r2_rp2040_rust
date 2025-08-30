use core::fmt::Debug;
use core::future::IntoFuture;

use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::spi::Instance;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::DynamicReceiver;
use embassy_sync::mutex::Mutex;
use embassy_sync::pubsub::{PubSubChannel, Publisher};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};
use embedded_can::nb::Can;
use embedded_can::ExtendedId;
extern crate alloc;

use mcp25xx::bitrates::clock_16mhz::CNF_1000K_BPS;
use mcp25xx::registers::{OperationMode, CANINTE, RXB0CTRL, RXB1CTRL, RXM};
use mcp25xx::{AcceptanceFilter, Config, IdHeader, MCP25xx};

use embassy_futures::select::{select, select4, Either4};

use crate::SpiBusType;
use crate::can_consumer::{CanSimpleDispatcher, CanFrameConsumer};

//pub type CanTransciever<'a, T> = MCP25xx<SpiDevice<'static, CriticalSectionRawMutex, SpiBusType<'a, T>, Output<'static>>>;
pub type CanTransciever<'a, T> = MCP25xx<SpiDevice<'a, CriticalSectionRawMutex, SpiBusType<'a, T>, Output<'a>>>;
pub type CanTranscieverMutex<'a, T> = Mutex<CriticalSectionRawMutex, CanTransciever<'a, T>>;

pub(in crate) const CAP: usize = 64;
pub(in crate) const SUBS: usize = 1;
pub(in crate) const PUBS: usize = 2;

pub type ConfigurationEventChannelType = PubSubChannel<CriticalSectionRawMutex, ConfigurationEvent, CAP, SUBS, PUBS>;
pub type ConfigurationEventPublisherType<'a> = Publisher<'a, CriticalSectionRawMutex, ConfigurationEvent, CAP, SUBS, PUBS>;
pub static CONFIGURATION_CHANNEL: ConfigurationEventChannelType  = PubSubChannel::new();
pub static NEEDS_TICK_SIGNAL: Signal<CriticalSectionRawMutex, bool> = Signal::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationEvent {
    NodeIdUpdate { node_id: u32 },
    IntervalUpdate { hz: u64 },
}

fn configure_mcp25xx<T: Instance>(
    mcp25xx: &mut CanTransciever<T>, 
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

pub struct CanService<'a, const N: usize, BUS, F>
where
    BUS: embassy_rp::spi::Instance,
    //C: embedded_can::nb::Can<Frame = F>,
    F: embedded_can::Frame
{
    pub can_bus: CanTranscieverMutex<'a, BUS>,
    reset: Output<'a>,
    int: Input<'a>,
    node_id: u32,
    dispatcher: CanSimpleDispatcher<'a, N, F>,
    tx_subscriber: DynamicReceiver<'a, F>,
}

impl<'a, const N: usize, BUS> CanService<'a, N, BUS, mcp25xx::CanFrame>
where
    BUS: embassy_rp::spi::Instance,
    //C: embedded_can::nb::Can<Frame = F>,
    //F: embedded_can::Frame
{

    pub fn new(
        can_bus: CanTranscieverMutex<'a, BUS>,
        reset: Output<'a>,
        int: Input<'a>,
        node_id: u32,
        tx_subscriber: DynamicReceiver<'a, mcp25xx::CanFrame>,

    ) -> Self {
        let dispatcher: CanSimpleDispatcher<'_, N, mcp25xx::CanFrame> = CanSimpleDispatcher::new(node_id);

        Self {
            can_bus: can_bus,
            reset: reset,
            int: int,
            node_id: node_id,
            dispatcher: dispatcher,
            tx_subscriber: tx_subscriber
        }
    }

    pub fn register(&mut self, consumer: &'a mut (dyn CanFrameConsumer<mcp25xx::CanFrame> + 'a)) -> Result<(), ()> {
        self.dispatcher.register(consumer)
    }

    // Run Function
    pub async fn run(&mut self) {
        //Timer::after_secs(2).await;

        // Reset the MCP25xx chip
        self.reset.set_low();
        Timer::after_millis(100).await;
        self.reset.set_high(); // bring the chip out of reset

        {
            let mut mcp25xx = self.can_bus.lock().await;
            configure_mcp25xx(&mut mcp25xx, self.node_id);
        }

        // Set up an optional ticker variable. When present, this will trigger periodic events
        // which will cause the device to transmit the current angle.
        let mut ticker: Option<Ticker> = None;   // Default Rate is 1Hz

        let mut configuration_subscriber = CONFIGURATION_CHANNEL.subscriber().unwrap();

        loop {
            // Check if we need to process RX
            let (process_rx, process_configuration, tx_frame,  needs_tick) = 
                match select4(self.int.wait_for_low(), configuration_subscriber.next_message_pure(), self.tx_subscriber.receive(), NEEDS_TICK_SIGNAL.wait()).await {
                    Either4::First(_) => (true, None, None, false),                                 // received message
                    Either4::Second(vals) => (false, Some(vals), None, false),  // triggered from a settings update
                    Either4::Third(frame) => (false, None, Some(frame), false),           // indicate a frame needs to be transmitted
                    Either4::Fourth(_) => (false, None, None, true)                                 // indicates a tick request has been recieved 
                };

            // Handle configuration changes
            match process_configuration {
                Some(ConfigurationEvent::NodeIdUpdate { node_id: new_id }) => {
                    let mut mcp25xx = self.can_bus.lock().await;
                    log::info!("Update Node Id to {:?}", new_id);
                    self.node_id = new_id;
                    configure_mcp25xx(&mut mcp25xx, self.node_id);
                    self.dispatcher.set_node_id(self.node_id);
                },
                Some(ConfigurationEvent::IntervalUpdate { hz }) => { 
                    log::info!("Update Interval to {:?} Hz", hz);
                    ticker.replace(Ticker::every(Duration::from_hz(hz)));
                },
                None => {}
            }

            if process_rx {
                let maybe_frame = {
                    let mut guard = self.can_bus.lock().await;
                    guard.receive()
                };
                match maybe_frame {
                    Ok(frame) => self.dispatcher.dispatch(&frame).await,
                    _ => {}
                };
            }
            
            if tx_frame.is_some() {
                let f = tx_frame.unwrap();
                let mut mcp25xx = self.can_bus.lock().await;
                let _ = mcp25xx.transmit(&f);
            }

            if needs_tick {
                self.dispatcher.tick().await;    // Always Tick
            }
        }
    }

}
