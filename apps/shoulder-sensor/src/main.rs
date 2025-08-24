#![no_std]
#![no_main]

mod cli_commands;
mod tlv493d;
mod usb;

// Use of a mod or pub mod is not actually necessary.
pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

use core::ops::Range;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicI16, Ordering};

// Linked-List First Fit Heap allocator (feature = "llff")
use embedded_alloc::LlffHeap as Heap;

use defmt::unwrap;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::{Executor, Spawner};
use embassy_rp::flash;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals;
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::spi::{self, Instance, Spi};
use embassy_rp::{bind_interrupts, Peri};
use embassy_rp::watchdog::Watchdog;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, CriticalSectionRawMutex};
use embassy_sync::pubsub::PubSubChannel;
use embassy_time::{Duration, Ticker};
use embedded_can::blocking::Can;
use portable_atomic::AtomicU64;
use smart_leds::RGB8;
use static_cell::StaticCell;
use usb_cli;
use canbus::{SpiBusMutex, SpiBusType};
use canbus::can_updater::can_updater_task;

use embedded_can::{ExtendedId, Frame};

use no_std_moving_average::MovingAverage;

use canbus::can::can_handler;
use usb::usb_handler;

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;

use {defmt_rtt as _, panic_probe as _}; // global logger

extern crate alloc;

#[global_allocator]
static HEAP: Heap = Heap::empty();

static EXECUTOR0: StaticCell<Executor> = StaticCell::new();

static TLV_ANGLE: AtomicI16 = AtomicI16::new(0);
static TLV_TEMP: AtomicI16 = AtomicI16::new(0);
pub static UPTIME: AtomicU64 = AtomicU64::new(0);

const FLASH_SIZE: usize = 8 * 1024 * 1024;
const FLASH_RANGE: Range<u32> = 0x480000..0x500000;

type FlashType = embassy_rp::flash::Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;
type FlashMutex = Mutex<CriticalSectionRawMutex, FlashType>;
type I2c1Bus = Mutex<NoopRawMutex, I2c<'static, peripherals::I2C1, i2c::Async>>;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<peripherals::PIO0>;
    I2C1_IRQ => I2cInterruptHandler<peripherals::I2C1>;
});

static SHARED_RX: PubSubChannel<CriticalSectionRawMutex, u8, 128, 1, 1> = PubSubChannel::new();
static SHARED_TX: PubSubChannel<CriticalSectionRawMutex, u8, 128, 1, 1> = PubSubChannel::new();

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

#[embassy_executor::task]
async fn cli_task(flash: &'static FlashMutex) {
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

    let tx = unwrap!(SHARED_TX.publisher());
    let rx = unwrap!(SHARED_RX.subscriber());
    usb_cli::cli_handler(rx, tx, commands, prompt).await;
}

#[embassy_executor::task]
async fn tlv493d_task(i2c_bus: &'static I2c1Bus) {
    // Set Up The TLV Sensor
    let i2c_dev = I2cDevice::new(i2c_bus);
    let mut sensor = tlv493d::Tlv493dDriver::new(i2c_dev, 0x5eu8, tlv493d::Mode::Master).await;
    
    let mut angle_avg = MovingAverage::<i16, i32, 20>::new();
    let mut temp_avg = MovingAverage::<i16, i32, 20>::new();

    // Fire Every 10ms (100Hz)
    let mut ticker = Ticker::every(Duration::from_millis(10));
    loop {
        let (angle, temp) = sensor.read_angle_and_temp_f32().await;
        let result_angle = angle_avg.average((angle * 100.0) as i16);
        let result_temp = temp_avg.average((temp * 100.0) as i16);
        TLV_ANGLE.store(result_angle, Ordering::Relaxed);
        TLV_TEMP.store(result_temp, Ordering::Relaxed);
        ticker.next().await;
    }
}

fn tx_report<T: Instance>(mcp25xx: &mut canbus::can::CanTransciever<T>, node_id: u32, sequence: &mut u8) {
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

    let frame = Frame::new(
        //Id::Extended(ExtendedId::ZERO),
        can_id,
        &data_bytes,
    );

    // If we successfully created the frame, add it to the transmit queue of the
    // CAN transceiver.
    match frame {
        None => {},
        Some(ref f) => match mcp25xx.transmit(f) {
            Ok(_) => { },
            //Err(_) => {},
            Err(error) => {
                log::info!("Transmit Error: {:?}", error);
            }
        }
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
    let usb_rx = unwrap!(SHARED_RX.publisher());
    let usb_tx = unwrap!(SHARED_TX.subscriber());
    unwrap!(spawner.spawn(usb_handler(p.USB, usb_rx, usb_tx)));

    // Set Up Colour Wheel Indicator
    // adafruit rp2040 CAN BUST Feather
    let mut neopixel_power = Output::new(p.PIN_20, Level::High);
    neopixel_power.set_high();
    unwrap!(spawner.spawn(colour_wheel(p.PIO0, p.DMA_CH0, p.PIN_21)));

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
    unwrap!(spawner.spawn(can_handler(spi_bus, can_cs, can_reset, can_int, flash, FLASH_RANGE, tx_report)));

    // Set Up The CLI Task
    unwrap!(spawner.spawn(cli_task(flash)));

    // Set Up The Can Updater Task
    unwrap!(spawner.spawn(can_updater_task(flash)));

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
