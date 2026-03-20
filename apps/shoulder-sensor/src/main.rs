#![no_std]
#![no_main]

mod can_tasks;
mod cli_commands;
mod cli_task;
mod tlv493d;

// Use of a mod or pub mod is not actually necessary.
pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

use core::ops::Range;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicI16, Ordering};

extern crate alloc;
use embassy_sync::pipe::Pipe;
// Linked-List First Fit Heap allocator (feature = "llff")
use embedded_alloc::LlffHeap as Heap;

use defmt::unwrap;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_executor::raw::Executor as RawExecutor;
use embassy_rp::flash;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals;
use embassy_rp::spi::{self, Spi};
use embassy_rp::bind_interrupts;
use embassy_rp::watchdog::Watchdog;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_time::{Duration, Instant, Ticker};
use portable_atomic::AtomicU64;
use signalo_filters::traits::WithConfig;
use static_cell::StaticCell;
use usb_cli;
use canbus::{SpiBusMutex, SpiBusType};
use canbus::can_updater::can_updater_task;
use usb_cli::cpu_handler::GLOBAL_CPU_LOADS;

use can_tasks::{can_handler, can_reporter};
use cli_task::cli_task;

use usb_serial::usb_handler;
use usb_serial::UsbPipe;

use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;

use signalo_filters::traits::Filter;
use signalo_filters::mean::mean::Mean;
use signalo_filters::median::Median;
use signalo_filters::observe::alpha_beta::{AlphaBeta, Config as AlphaBetaConfig};

use {defmt_rtt as _, panic_probe as _}; // global logger

#[global_allocator]
static HEAP: Heap = Heap::empty();

static TLV_ANGLE: AtomicI16 = AtomicI16::new(0);
static TLV_TEMP: AtomicI16 = AtomicI16::new(0);
pub static UPTIME: AtomicU64 = AtomicU64::new(0);

const FLASH_SIZE: usize = 8 * 1024 * 1024;
const FLASH_RANGE: Range<u32> = 0x480000..0x500000;

type FlashType = embassy_rp::flash::Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;
type FlashMutex = Mutex<CriticalSectionRawMutex, FlashType>;
type I2c1Bus = Mutex<NoopRawMutex, I2c<'static, peripherals::I2C1, i2c::Async>>;

bind_interrupts!(struct Irqs {
    I2C1_IRQ => I2cInterruptHandler<peripherals::I2C1>;
});

#[embassy_executor::task]
async fn cpu_usage() {
    let mut previous_tick = 0u64;
    let mut previous_sleep_tick = 0u64;
    let mut ticker = Ticker::every(Duration::from_millis(1000));
    loop {
        let current_tick = Instant::now().as_ticks();
        let current_sleep_tick = SLEEP_TICKS.load(Ordering::Relaxed);
        let sleep_tick_difference = (current_sleep_tick - previous_sleep_tick) as f32;
        let tick_difference = (current_tick - previous_tick) as f32;
        let usage = 1f32 - sleep_tick_difference / tick_difference;
        previous_tick = current_tick;
        previous_sleep_tick = current_sleep_tick;

        //log::info!("Cpu usage: {}%", usage * 100f32);
        GLOBAL_CPU_LOADS.lock(|cell| {
            let mut loads = cell.get();
            loads.update(usage * 100.0);
            cell.set(loads);
        });
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn tlv493d_task(i2c_bus: &'static I2c1Bus) {
    // Set Up The TLV Sensor
    let i2c_dev = I2cDevice::new(i2c_bus);
    let mut sensor = tlv493d::Tlv493dDriver::new(i2c_dev, 0x5eu8, tlv493d::Mode::Master).await;

    // Temperature Averaging
    let mut temp_median_filter: Median<f32, 3> = Median::default();
    let mut temp_mean_filter: Mean<f32, 20> = Mean::default();

    let mut median_filter: Median<f32, 3> = Median::default();
    let mut mean_filter: Mean<f32, 20> = Mean::default();
    let mut angle_ab = AlphaBeta::with_config(AlphaBetaConfig {
        alpha: 0.15f32,
        beta: 0.008f32,
    });

    // Fire Every 0.5ms (2000Hz)
    let mut ticker = Ticker::every(Duration::from_hz(2000));
    let mut iteration = 0;
    loop {
        let (rad, _angle, temp) = sensor.read_angle_and_temp_f32().await;

        let median_temp = temp_median_filter.filter(temp);
        let mean_temp = temp_mean_filter.filter(median_temp);

        let median_rad = median_filter.filter(rad);
        let mean_angle = mean_filter.filter(median_rad);

        if iteration == 0 {
            let result_angle_rad = angle_ab.filter(mean_angle);
            TLV_ANGLE.store((result_angle_rad.to_degrees() * 100.0) as i16, Ordering::Relaxed);
            TLV_TEMP.store((mean_temp * 100.0) as i16, Ordering::Relaxed);
        }

        iteration = (iteration + 1) % 20;
        ticker.next().await;
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

    // I2C Setup
    log::info!("set up i2c ");
    let i2c_config = {
        let mut config = i2c::Config::default();
        config.frequency = 400_000;
        config
    };
    let i2c = i2c::I2c::new_async(p.I2C1, p.PIN_3, p.PIN_2, Irqs, i2c_config);
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

    unwrap!(spawner.spawn(cpu_usage()));

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

static EXECUTOR: StaticCell<RawExecutor> = StaticCell::new();
static SLEEP_TICKS: AtomicU64 = AtomicU64::new(0);

#[cortex_m_rt::entry]
fn main() -> ! {
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 1280;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    // Set Up The Executor
    //let executor0 = EXECUTOR0.init(Executor::new());
    //executor0.run(|spawner| unwrap!(spawner.spawn(core0_task(spawner))));
    
    let raw_executor = EXECUTOR.init(RawExecutor::new(usize::MAX as *mut ()));
    let spawner = raw_executor.spawner();
    unwrap!(spawner.spawn(core0_task(spawner)));
    loop {
        let before = Instant::now().as_ticks();
        cortex_m::asm::wfe();
        let after = Instant::now().as_ticks();
        SLEEP_TICKS.fetch_add(after - before, Ordering::Relaxed);
        unsafe { raw_executor.poll() };
    }
}
