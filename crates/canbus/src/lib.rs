#![no_std]

#[cfg(feature = "usb-cli")]
pub mod can_cli_commands;

pub mod can_consumer;
pub mod can_updater;
pub mod can;
pub mod isotp;
mod util;

extern crate alloc;
use core::cell::RefCell;
use embassy_rp::flash;
use embassy_rp::peripherals;
use embassy_rp::spi::{self, Spi};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

const FLASH_SIZE: usize = 8 * 1024 * 1024;

// -- Types for use
type FlashType = embassy_rp::flash::Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;
type FlashMutex = embassy_sync::mutex::Mutex<CriticalSectionRawMutex, FlashType>;

pub type SpiBusType<'a, T> = Spi<'a, T, spi::Blocking>;
pub type SpiBusMutex<'a, T> = Mutex<CriticalSectionRawMutex, RefCell<SpiBusType<'a,  T>>>;

//pub type TxReportHandler<'a, T: Instance> = fn(&mut CanTransciever<T>, u32, &mut u8);

pub type TxReportHandler<'a, T> = fn(&mut can::CanTransciever<T>, u32, &mut u8);