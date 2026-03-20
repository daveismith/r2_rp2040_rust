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
use embassy_executor::raw::Executor as RawExecutor;
use embassy_rp::flash;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::multicore::{spawn_core1, Stack};
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
use usb_cli::cpu_handler::{ GLOBAL_CPU0_LOADS, GLOBAL_CPU1_LOADS };

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
    let mut previous_sleep0_tick = 0u64;
    let mut previous_sleep1_tick = 0u64;
    let mut ticker = Ticker::every(Duration::from_millis(1000));
    loop {
        let current_tick = Instant::now().as_ticks();
        let current_sleep0_tick = SLEEP_TICKS_0.load(Ordering::Relaxed);
        let current_sleep1_tick = SLEEP_TICKS_1.load(Ordering::Relaxed);

        let sleep0_tick_difference = (current_sleep0_tick - previous_sleep0_tick) as f32;
        let sleep1_tick_difference = (current_sleep1_tick - previous_sleep1_tick) as f32;

        let tick_difference = (current_tick - previous_tick) as f32;
        let usage0 = 1f32 - sleep0_tick_difference / tick_difference;
        let usage1 = 1f32 - sleep1_tick_difference / tick_difference;

        previous_tick = current_tick;
        previous_sleep0_tick = current_sleep0_tick;
        previous_sleep1_tick = current_sleep1_tick;

        //log::info!("Cpu usage: {}%", usage * 100f32);
        GLOBAL_CPU0_LOADS.lock(|cell| {
            let mut loads = cell.get();
            loads.update(usage0 * 100.0);
            cell.set(loads);
        });

        GLOBAL_CPU1_LOADS.lock(|cell| {
            let mut loads = cell.get();
            loads.update(usage1 * 100.0);
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
    let mut temp_mean_filter: Mean<f32, 200> = Mean::default();

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
#[embassy_executor::task]
async fn my_main(mut watchdog: Watchdog) {
    //let p = embassy_rp::init(Default::default());
    // The core loop
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        watchdog.feed();
        ticker.next().await;
        UPTIME.add(1u64, Ordering::AcqRel);
    }
}

#[embassy_executor::task]
async fn core1_task() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        log::info!("Hello from core 1!");
        ticker.next().await;
    }
}

static EXECUTOR_0: StaticCell<RawExecutor> = StaticCell::new();
static SLEEP_TICKS_0: AtomicU64 = AtomicU64::new(0);

static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR_1: StaticCell<RawExecutor> = StaticCell::new();
static SLEEP_TICKS_1: AtomicU64 = AtomicU64::new(0);

#[cortex_m_rt::entry]
fn main() -> ! {
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 1280;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

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
 
     // I2C Setup
    let i2c_config = {
        let mut config = i2c::Config::default();
        config.frequency = 400_000;
        config
    };
    let i2c = i2c::I2c::new_async(p.I2C1, p.PIN_3, p.PIN_2, Irqs, i2c_config);
    static I2C_BUS: StaticCell<I2c1Bus> = StaticCell::new();
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

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

    // Set Up The Core 1 Executor
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor = EXECUTOR_1.init(RawExecutor::new(usize::MAX as *mut ()));
            let spawner = executor.spawner();

            unwrap!(spawner.spawn(tlv493d_task(i2c_bus)));
            executor_loop_sync(executor, &SLEEP_TICKS_1)
        },
    );


    // Set Up The Core 0 Executor
    let core0_executor = EXECUTOR_0.init(RawExecutor::new(usize::MAX as *mut ()));
    let spawner = core0_executor.spawner();

    unwrap!(spawner.spawn(usb_handler(p.USB, "test", usb_rx_writer, usb_tx_reader)));
    //unwrap!(spawner.spawn(tlv493d_task(i2c_bus)));
    unwrap!(spawner.spawn(can_handler(spi_bus, can_cs, can_reset, can_int, flash, FLASH_RANGE)));
    unwrap!(spawner.spawn(cli_task(flash, usb_tx_writer, usb_rx_reader)));
    unwrap!(spawner.spawn(can_updater_task(flash)));     // Set Up The Can Updater Task
    unwrap!(spawner.spawn(can_reporter()));
    unwrap!(spawner.spawn(cpu_usage()));
    unwrap!(spawner.spawn(my_main(watchdog)));

    executor_loop_sync(core0_executor, &SLEEP_TICKS_0);
}

fn executor_loop_sync(executor: &'static RawExecutor, sleep_tick_count: &AtomicU64) -> ! {
    loop {
        let before = Instant::now().as_ticks();
        cortex_m::asm::wfe();
        let after = Instant::now().as_ticks();
        sleep_tick_count.fetch_add(after - before, Ordering::Relaxed);
        unsafe { executor.poll() };
    }
}
