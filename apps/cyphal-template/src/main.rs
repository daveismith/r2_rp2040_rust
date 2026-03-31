#![no_std]
#![no_main]

mod can_tasks;
mod can_driver;
mod cli_commands;
mod cli_task;
mod identify_led;

extern crate alloc;
use can_tasks::can_handler;
use core::cell::RefCell;
use core::ptr::addr_of_mut;
use core::sync::atomic::Ordering;

use defmt::unwrap;
use embassy_executor::raw::Executor as RawExecutor;
use embassy_rp::bind_interrupts;
use embassy_rp::flash::{self, Flash};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::multicore::{spawn_core1, Stack};
use embassy_rp::peripherals;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::spi::{self, Spi};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::Peri;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Duration, Instant, Ticker};
// Linked-List First Fit Heap allocator (feature = "llff")
use embedded_alloc::LlffHeap as Heap;
use portable_atomic::AtomicU64;
use smart_leds::RGB8;
use static_cell::StaticCell;

use cli_task::cli_task;
use usb_cli::cpu_handler::{ GLOBAL_CPU0_LOADS, GLOBAL_CPU1_LOADS };
use usb_serial::usb_handler;
use usb_serial::UsbPipe;

use {defmt_rtt as _, panic_probe as _}; // global logger


// Use of a mod or pub mod is not actually necessary.
pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[global_allocator]
static HEAP: Heap = Heap::empty();

pub static UPTIME: AtomicU64 = AtomicU64::new(0);

pub type SpiBusType<'a, T> = Spi<'a, T, spi::Blocking>;
pub type SpiBusMutex<'a, T> = BlockingMutex<CriticalSectionRawMutex, RefCell<SpiBusType<'a, T>>>;

static EXECUTOR_0: StaticCell<RawExecutor> = StaticCell::new();
static SLEEP_TICKS_0: AtomicU64 = AtomicU64::new(0);

static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR_1: StaticCell<RawExecutor> = StaticCell::new();
static SLEEP_TICKS_1: AtomicU64 = AtomicU64::new(0);

const FLASH_SIZE: usize = 8 * 1024 * 1024;
const NVS_RANGE: core::ops::Range<u32> = 0x480000..0x500000;

pub type FlashType = Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;
pub type FlashMutex = Mutex<CriticalSectionRawMutex, FlashType>;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<peripherals::PIO0>;
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
async fn my_main(mut watchdog: Watchdog, mut led: Output<'static>) {
    //let p = embassy_rp::init(Default::default());
    // The core loop
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        led.toggle();
        watchdog.feed();
        ticker.next().await;
        UPTIME.add(1u64, Ordering::AcqRel);
    }
}

#[embassy_executor::task]
async fn core1_task() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        //log::info!("Hello from core 1!");
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn identify_led_task(
    mut led_power: Output<'static>,
    pio0: Peri<'static, peripherals::PIO0>,
    dma_ch0: Peri<'static, peripherals::DMA_CH0>,
    data_pin: Peri<'static, peripherals::PIN_21>,
) {
    led_power.set_high();

    let Pio {
        mut common, sm0, ..
    } = Pio::new(pio0, Irqs);
    let program = PioWs2812Program::new(&mut common);
    let mut ws2812 = PioWs2812::<_, 0, 1>::new(&mut common, sm0, dma_ch0, data_pin, &program);

    let mut pixel = [RGB8::default(); 1];
    let mut wheel_phase: u8 = 0;
    let mut was_identifying = false;
    loop {
        let identifying = identify_led::is_identifying();
        if identifying && !was_identifying {
            log::info!("identify: animation start");
        } else if !identifying && was_identifying {
            log::info!("identify: animation stop");
        }
        was_identifying = identifying;

        if identifying {
            let (r, g, b) = identify_led::wheel_color(wheel_phase);
            pixel[0] = RGB8 { r, g, b };
            wheel_phase = wheel_phase.wrapping_add(3);
        } else {
            pixel[0] = RGB8::default();
        }

        ws2812.write(&pixel).await;
        embassy_time::Timer::after_millis(40).await;
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 16 * 1024;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    let p = embassy_rp::init(Default::default());

    // Set up shared flash and read unique ID before concurrent tasks start using it.
    let mut flash = embassy_rp::flash::Flash::<_, _, FLASH_SIZE>::new(p.FLASH, p.DMA_CH1);
    let node_unique_id = read_unique_id(&mut flash);
    static FLASH: StaticCell<FlashMutex> = StaticCell::new();
    let flash = FLASH.init(Mutex::new(flash));
   
    // Override bootloader watchdog
    let mut watchdog = Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(8));
    watchdog.feed();

    // LED
    let led = Output::new(p.PIN_25, Level::Low);

    // Set Up The USB Handler
    static SHARED_RX_PIPE: StaticCell<UsbPipe> = StaticCell::new();
    static SHARED_TX_PIPE: StaticCell<UsbPipe> = StaticCell::new();
    let rx_pipe = SHARED_RX_PIPE.init(Pipe::new());
    let tx_pipe = SHARED_TX_PIPE.init(Pipe::new());
    let (usb_rx_reader, usb_rx_writer) = rx_pipe.split();
    let (usb_tx_reader, usb_tx_writer) = tx_pipe.split();
 
    // The feather has a MCP25625, charge bay has MCP2515
    // CAN is SPI0.
    // 3MHz seems to be the fastest that this runs out of the box.
    let mut config = spi::Config::default();
    config.frequency = 3_000_000;

    // Setup SPI bus
    let spi = Spi::new_blocking(p.SPI1, p.PIN_14, p.PIN_15, p.PIN_8, config);
    let spi_bus: BlockingMutex<CriticalSectionRawMutex, RefCell<SpiBusType<'_, peripherals::SPI1>>>  = BlockingMutex::new(RefCell::new(spi));
    static MY_SPI_BUS: StaticCell<SpiBusMutex<peripherals::SPI1>> = StaticCell::new();
    let spi_bus = MY_SPI_BUS.init(spi_bus);
    let can_cs = Output::new(p.PIN_19, Level::High);
    let can_reset = Output::new(p.PIN_18, Level::Low);
    let can_int = Input::new(p.PIN_22, Pull::Up);
    let led_power = Output::new(p.PIN_20, Level::Low);

    // Set Up The Core 1 Executor
    spawn_core1(
        p.CORE1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            let executor = EXECUTOR_1.init(RawExecutor::new(usize::MAX as *mut ()));
            let spawner = executor.spawner();

            //unwrap!(spawner.spawn(tlv493d_task(i2c_bus)));
            unwrap!(spawner.spawn(core1_task()));
            executor_loop_sync(executor, &SLEEP_TICKS_1)
        },
    );


    // Set Up The Core 0 Executor
    let core0_executor = EXECUTOR_0.init(RawExecutor::new(usize::MAX as *mut ()));
    let spawner = core0_executor.spawner();

    unwrap!(spawner.spawn(usb_handler(p.USB, "test", usb_rx_writer, usb_tx_reader)));
    unwrap!(spawner.spawn(identify_led_task(led_power, p.PIO0, p.DMA_CH0, p.PIN_21)));
    unwrap!(spawner.spawn(can_handler(spi_bus, can_cs, can_reset, can_int, flash, NVS_RANGE, node_unique_id)));
    unwrap!(spawner.spawn(cli_task(usb_tx_writer, usb_rx_reader)));
    unwrap!(spawner.spawn(cpu_usage()));
    unwrap!(spawner.spawn(my_main(watchdog, led)));

    executor_loop_sync(core0_executor, &SLEEP_TICKS_0);
}

fn read_unique_id(flash: &mut FlashType) -> [u8; 16] {
    let mut unique_id = [0u8; 16];
    let mut flash_id = [0u8; 8];
    if flash.blocking_unique_id(&mut flash_id).is_ok() {
        unique_id[..8].copy_from_slice(&flash_id);
    }
    unique_id
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