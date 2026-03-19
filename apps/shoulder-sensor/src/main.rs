#![no_std]
#![no_main]

mod cli_commands;
mod tlv493d;

// Use of a mod or pub mod is not actually necessary.
pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

use core::ops::{DerefMut, Range};
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicI16, Ordering};

extern crate alloc;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_sync::pipe::Pipe;
// Linked-List First Fit Heap allocator (feature = "llff")
use embedded_alloc::LlffHeap as Heap;

use defmt::unwrap;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::{Executor, Spawner};
use embassy_rp::flash;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals;
//use embassy_rp::pio::{InterruptHandler, Pio};
//use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::spi::{self, Spi};
//use embassy_rp::{bind_interrupts, Peri};
use embassy_rp::bind_interrupts;
use embassy_rp::watchdog::Watchdog;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_time::{Duration, Ticker};
use mcp25xx::{CanFrame, MCP25xx};
use portable_atomic::AtomicU64;
use sequential_storage::cache::NoCache;
use sequential_storage::map::fetch_item;
use signalo_filters::traits::WithConfig;
//use smart_leds::RGB8;
use static_cell::StaticCell;
use usb_cli;
use canbus::{SpiBusMutex, SpiBusType, CAN_NODE_ID};
use canbus::can_updater::{can_updater_task, CanFirmwareUpdater};

use embedded_can::{ExtendedId, Frame};

use no_std_moving_average::MovingAverage;

use usb_serial::usb_handler;
use usb_serial::{UsbPipe, UsbPipeReader, UsbPipeWriter};

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;

use signalo_filters::traits::Filter;
use signalo_filters::median::Median;
use signalo_filters::observe::alpha_beta::{AlphaBeta, Config as AlphaBetaConfig};

use {defmt_rtt as _, panic_probe as _}; // global logger

#[global_allocator]
static HEAP: Heap = Heap::empty();

static EXECUTOR0: StaticCell<Executor> = StaticCell::new();

static TLV_ANGLE: AtomicI16 = AtomicI16::new(0);
static TLV_TEMP: AtomicI16 = AtomicI16::new(0);
pub static UPTIME: AtomicU64 = AtomicU64::new(0);

const FLASH_SIZE: usize = 8 * 1024 * 1024;
const FLASH_RANGE: Range<u32> = 0x480000..0x500000;
const DEFAULT_NODE_ID: u32 = 0;

type FlashType = embassy_rp::flash::Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;
type FlashMutex = Mutex<CriticalSectionRawMutex, FlashType>;
type I2c1Bus = Mutex<NoopRawMutex, I2c<'static, peripherals::I2C1, i2c::Async>>;

static TX_QUEUE: embassy_sync::channel::Channel<CriticalSectionRawMutex, mcp25xx::CanFrame, 4> = embassy_sync::channel::Channel::new();

bind_interrupts!(struct Irqs {
    //PIO0_IRQ_0 => InterruptHandler<peripherals::PIO0>;
    I2C1_IRQ => I2cInterruptHandler<peripherals::I2C1>;
});

/*
/// Input a value 0 to 255 to get a color value
/// The colours are a transition r - g - b - back to r.
fn wheel(mut wheel_pos: u8) -> RGB8 {
    wheel_pos = 255 - wheel_pos;
    if wheel_pos < 85 {
        return (255 - wheel_pos * 3, 0, wheel_pos * 3).into();
    }
    if wheel_pos < 170 {
        wheel_pos -= 85;
        return (0, wheel_pos * 3, 255 - wheel_pos * 3).into();
    }
    wheel_pos -= 170;
    (wheel_pos * 3, 255 - wheel_pos * 3, 0).into()
}

#[embassy_executor::task]
async fn colour_wheel(
    pio: Peri<'static, peripherals::PIO0>,
    dma: Peri<'static, peripherals::DMA_CH0>,
    pin: Peri<'static, peripherals::PIN_21>,
) {
    // Set Up The PIO & Colour Wheel
    let Pio {
        mut common, sm0, ..
    } = Pio::new(pio, Irqs);

    // This is the number of leds in the string. Helpfully, the sparkfun thing plus and adafruit
    // feather boards for the 2040 both have one built in.
    const NUM_LEDS: usize = 1;
    let mut data = [RGB8::default(); NUM_LEDS];

    // Common neopixel pins:
    // Thing plus: 8
    // Adafruit Feather: 16;  Adafruit Feather+RFM95: 4
    let program = PioWs2812Program::new(&mut common);
    let mut ws2812 = PioWs2812::new(&mut common, sm0, dma, pin, &program);

    // Main Loop
    let mut ticker = Ticker::every(Duration::from_millis(10));
    loop {
        for j in 0..(256 * 5) {
            log::debug!("New Colors:");
            for i in 0..NUM_LEDS {
                data[i] = wheel((((i * 256) as u16 / NUM_LEDS as u16 + j as u16) & 255) as u8);
                log::debug!("R: {} G: {} B: {}", data[i].r, data[i].g, data[i].b);
            }
            ws2812.write(&data).await;

            ticker.next().await;
        }
    }
}
*/

#[embassy_executor::task]
async fn cli_task(flash: &'static FlashMutex,
    tx: UsbPipeWriter<'static>,
    rx: UsbPipeReader<'static>
) {
    // Build the command registry
    let version = usb_cli::Command::new("version", "Print Version Details", cli_commands::VersionCommand);
    let echo = usb_cli::Command::new("echo", "Echo input", usb_cli::handlers::EchoCommand);
    let bootload = usb_cli::Command::new("bootload", "Launch USB Bootloader", usb_cli::handlers::BootloadCommand);
    let restart = usb_cli::Command::new("restart", "Restart the system", usb_cli::handlers::RestartCommand);
    
    //Specific 
    let uptime = usb_cli::Command::new("uptime", "Check uptime of the device", cli_commands::UptimeCommand);
    let angle = usb_cli::Command::new("angle", "Read sensor angle", cli_commands::AngleCommand);
    let temp = usb_cli::Command::new("temp", "Read sensor temperature", cli_commands::TempCommand);
    let can = usb_cli::Command::new("can", "Configure CAN Bus", canbus::can_cli_commands::CanCommand::new(flash, &FLASH_RANGE));

    // Create the dispatcher with the registry.
    let commands = &[version, echo, uptime, angle, temp, can, bootload, restart, ];

    let prompt = "> ";

    usb_cli::cli_handler(tx, rx, commands, prompt).await;
}

#[embassy_executor::task]
async fn tlv493d_task(i2c_bus: &'static I2c1Bus) {
    // Set Up The TLV Sensor
    let i2c_dev = I2cDevice::new(i2c_bus);
    let mut sensor = tlv493d::Tlv493dDriver::new(i2c_dev, 0x5eu8, tlv493d::Mode::Master).await;
    
    //let mut angle_avg = MovingAverage::<i16, i32, 20>::new();
    let mut temp_avg = MovingAverage::<i16, i32, 20>::new();

    let mut median_filter: Median<f32, 3> = Median::default();
    let mut angle_ab = AlphaBeta::with_config(AlphaBetaConfig {
        alpha: 0.05f32,
        beta: 0.00128f32 / 50.0,
    });

    // Fire Every 10ms (100Hz)
    let mut ticker = Ticker::every(Duration::from_millis(20));
    loop {
        let (rad, _angle, temp) = sensor.read_angle_and_temp_f32().await;
        //let result_angle = angle_avg.average((angle * 100.0) as i16);
        let result_temp = temp_avg.average((temp * 100.0) as i16);

        let median_angle = median_filter.filter(rad);
        let result_angle_rad = angle_ab.filter(median_angle);
        let result_angle_ab = result_angle_rad.to_degrees();
        
        TLV_ANGLE.store((result_angle_ab * 100.0) as i16, Ordering::Relaxed);
        TLV_TEMP.store(result_temp, Ordering::Relaxed);
        ticker.next().await;
    }
}

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

//#[embassy_executor::main]
async fn my_main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // Override bootloader watchdog
    let mut watchdog = Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(8));
    watchdog.feed();

    // Set Up The Flash Peripheral For Sharing
    let flash = embassy_rp::flash::Flash::<_, _, FLASH_SIZE>::new(p.FLASH, p.DMA_CH1);
    static FLASH: StaticCell<FlashMutex> = StaticCell::new();
    let flash = FLASH.init(Mutex::new(flash));

    // Set Up The USB Handler
    static SHARED_RX_PIPE: StaticCell<UsbPipe> = StaticCell::new();
    static SHARED_TX_PIPE: StaticCell<UsbPipe> = StaticCell::new();


    let rx_pipe = SHARED_RX_PIPE.init(Pipe::new());
    let tx_pipe = SHARED_TX_PIPE.init(Pipe::new());

    let (usb_rx_reader, usb_rx_writer) = rx_pipe.split();
    let (usb_tx_reader, usb_tx_writer) = tx_pipe.split();
    unwrap!(spawner.spawn(usb_handler(p.USB, "test", usb_rx_writer, usb_tx_reader)));

    // Set Up Colour Wheel Indicator
    // adafruit rp2040 CAN BUST Feather
    //let mut neopixel_power = Output::new(p.PIN_20, Level::High);
    //neopixel_power.set_high();
    //unwrap!(spawner.spawn(colour_wheel(p.PIO0, p.DMA_CH0, p.PIN_21)));

    // I2C Setup
    log::info!("set up i2c ");
    let i2c = i2c::I2c::new_async(p.I2C1, p.PIN_3, p.PIN_2, Irqs, i2c::Config::default());
    static I2C_BUS: StaticCell<I2c1Bus> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    unwrap!(spawner.spawn(tlv493d_task(i2c_bus)));

    // The feather has a MCP25625, charge bay has MCP2515
    // CAN is SPI0.
    // 3MHz seems to be the fastest that this runs out of the box.
    let mut config = spi::Config::default();
    config.frequency = 3_0000_000; // 1MHz

    // Setup SPI bus
    let spi = Spi::new_blocking(p.SPI1, p.PIN_14, p.PIN_15, p.PIN_8, config);
    let spi_bus: BlockingMutex<CriticalSectionRawMutex, RefCell<SpiBusType<'_, peripherals::SPI1>>>  = BlockingMutex::new(RefCell::new(spi));
    static MY_SPI_BUS: StaticCell<SpiBusMutex<peripherals::SPI1>> = StaticCell::new();
    let spi_bus = MY_SPI_BUS.init(spi_bus);
    let can_cs = Output::new(p.PIN_19, Level::High);
    let can_reset = Output::new(p.PIN_18, Level::Low);
    let can_int = Input::new(p.PIN_22, Pull::None);
    unwrap!(spawner.spawn(can_handler(spi_bus, can_cs, can_reset, can_int, flash, FLASH_RANGE)));

    // Set Up The CLI Task
    unwrap!(spawner.spawn(cli_task(flash, usb_tx_writer, usb_rx_reader)));

    // Set Up The Can Updater Task
    unwrap!(spawner.spawn(can_updater_task(flash)));
    unwrap!(spawner.spawn(can_reporter()));

    // The core loop
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        watchdog.feed();
        ticker.next().await;
        UPTIME.add(1u64, Ordering::AcqRel);
    }
}

#[embassy_executor::task]
async fn core0_task(spawner: Spawner) {
    my_main(spawner).await
}

#[cortex_m_rt::entry]
fn main() -> ! {
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 1280;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    // Set Up The Executor
    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| unwrap!(spawner.spawn(core0_task(spawner))));
}
