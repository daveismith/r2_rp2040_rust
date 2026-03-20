use alloc::boxed::Box;
use async_trait::async_trait;
use embassy_time::Timer;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use core::cell::Cell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use crate::CommandHandler;

// Precalculated constants for e^(-1/W)
const EXP_5: f32 = 0.81873;
const EXP_15: f32 = 0.93551;
const EXP_60: f32 = 0.98347;
const EXP_300: f32 = 0.99667;
const EXP_600: f32 = 0.99833;
const EXP_900: f32 = 0.99889;

#[derive(Clone, Copy)] // Add this
pub struct LoadAverages {
    pub load_5s: f32,
    pub load_15s: f32,
    pub load_60s: f32,
    pub load_300s: f32,
    pub load_600s: f32,
    pub load_900s: f32,
}

impl LoadAverages {
    pub const fn new() -> Self {
        // Initialize to 0.0 (or 100.0 if you want to assume max load on boot)
        Self {
            load_5s: 0.0,
            load_15s: 0.0,
            load_60s: 0.0,
            load_300s: 0.0,
            load_600s: 0.0,
            load_900s: 0.0,
        }
    }

    /// Call this exactly once per second with the CPU percentage (0.0 to 100.0)
    pub fn update(&mut self, current_cpu_percent: f32) {
        self.load_5s = self.load_5s * EXP_5 + current_cpu_percent * (1.0 - EXP_5);
        self.load_15s = self.load_15s * EXP_15 + current_cpu_percent * (1.0 - EXP_15);
        self.load_60s = self.load_60s * EXP_60 + current_cpu_percent * (1.0 - EXP_60);
        self.load_300s = self.load_300s * EXP_300 + current_cpu_percent * (1.0 - EXP_300);
        self.load_600s = self.load_600s * EXP_600 + current_cpu_percent * (1.0 - EXP_600);
        self.load_900s = self.load_900s * EXP_900 + current_cpu_percent * (1.0 - EXP_900);
    }
}

// Initialize the global mutex with our struct
pub static GLOBAL_CPU0_LOADS: Mutex<CriticalSectionRawMutex, Cell<LoadAverages>> = 
    Mutex::new(Cell::new(LoadAverages::new()));
pub static GLOBAL_CPU1_LOADS: Mutex<CriticalSectionRawMutex, Cell<LoadAverages>> = 
    Mutex::new(Cell::new(LoadAverages::new()));
pub struct CpuCommand;

impl CpuCommand {
    
    async fn print_cpu_usage<IO>(&self, io: &mut IO, cpu: usize, loads: &LoadAverages)
    where
        IO: AsyncWrite + FmtWrite + Send,
    {
        writeln!(io, "CPU{} Usage:", cpu).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  5s: {:.2}%", loads.load_5s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  15s: {:.2}%", loads.load_15s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  60s: {:.2}%", loads.load_60s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  300s: {:.2}%", loads.load_300s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  600s: {:.2}%", loads.load_600s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run
        writeln!(io, "  900s: {:.2}%", loads.load_900s).ok();
        Timer::after_micros(100).await; // Yield to allow other tasks to run

    }

}

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for CpuCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{
    
    async fn execute(&self, _args: &[&str], io: &mut IO) {
        /*let cpu_freq = embassy_rp::system::SystemClock::cpu_freq();
        let freq_mhz = cpu_freq.integer() / 1_000_000;
        writeln!(io, "CPU Frequency: {} MHz", freq_mhz).ok();*/
        let current_loads = GLOBAL_CPU0_LOADS.lock(|cell| cell.get());
        self.print_cpu_usage(io, 0, &current_loads).await;

        let current_loads = GLOBAL_CPU1_LOADS.lock(|cell| cell.get());
        self.print_cpu_usage(io, 1, &current_loads).await;
    }
}