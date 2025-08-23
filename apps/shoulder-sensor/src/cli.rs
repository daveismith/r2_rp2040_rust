use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use core::ops::DerefMut;
use embedded_io_async::Write as AsyncWrite;
use heapless::Vec;
use embassy_time::Timer;

use sequential_storage::cache::NoCache;
use sequential_storage::map;

use crate::util::ParseSettingsError;
use crate::util;
use crate::{FlashMutex, FLASH_RANGE};

use crate::can::{ConfigurationEvent, CONFIGURATION_CHANNEL, ConfigurationEventPublisherType};

const MAX_ARGS: usize = 8;

// --- Trait for Command Handlers ---

#[async_trait(?Send)]
pub trait CommandHandler<IO>: Send + Sync {
    async fn execute(&self, args: &[&str], io: &mut IO);
}

// --- Command Struct ---

pub struct Command<IO> {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: Box<dyn CommandHandler<IO>>,
}

impl<IO> Command<IO> {
    pub fn new<H>(name: &'static str, description: &'static str, handler: H) -> Self
    where
        H: CommandHandler<IO> + 'static,
    {
        Self {
            name,
            description,
            handler: Box::new(handler),
        }
    }
}

// --- Command Dispatcher ---

pub struct CommandDispatcher<'a, IO> {
    commands: &'a [Command<IO>],
}

impl<'a, IO> CommandDispatcher<'a, IO>
where
    IO: AsyncWrite + FmtWrite,
{
    pub fn new(commands: &'a [Command<IO>]) -> Self {
        Self { commands }
    }

    pub async fn dispatch(&self, line: &str, io: &mut IO) {
        let mut args: Vec<&str, MAX_ARGS> = Vec::new();

        for arg in line.split_whitespace() {
            if args.push(arg).is_err() {
                break;
            }
        }

        if args.is_empty() {
            writeln!(io, "No command entered.").ok();
            return;
        }

        if args[0] == "help" {
            writeln!(io, "Available commands:").ok();
            for cmd in self.commands {
                writeln!(io, "  {:<10} - {}", cmd.name, cmd.description).ok();
                io.flush().await.ok();
                Timer::after_millis(10).await;
            }
            return;
        }

        for cmd in self.commands {
            if cmd.name == args[0] {
                cmd.handler.execute(&args, io).await;
                return;
            }
        }

        writeln!(io, "Unknown command: '{}'", args[0]).ok();
    }
}

// --- Example Commands ---

pub struct EchoCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for EchoCommand
where
    IO: AsyncWrite + FmtWrite,
{
    async fn execute(&self, args: &[&str], io: &mut IO) {
        if args.len() < 2 {
            writeln!(io, "Usage: echo <message>").ok();
            return;
        }

        let mut msg: heapless::String<64> = heapless::String::new();
        for (i, word) in args[1..].iter().enumerate() {
            if i > 0 {
                msg.push(' ').unwrap();
            }
            msg.push_str(word).unwrap();
        }

        writeln!(io, "content: {}", msg).ok();
    }
}

pub struct CanCommand {
    pub flash: &'static FlashMutex,
    pub publisher: ConfigurationEventPublisherType<'static>,
}

//TODO: Figure Out How To Manage This
impl<'a> CanCommand {

    pub fn new(flash: &'static FlashMutex) -> Self {
        Self { 
            flash: flash,
            publisher: CONFIGURATION_CHANNEL.publisher().unwrap()
        }
    }

}

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for CanCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{

    async fn execute(&self, args: &[&str], io: &mut IO) {
        let mut data_buffer: [u8; 128] = [0; 128];
        
        if args.len() < 2 {
            writeln!(io, "Usage: can [id|interval] ...").ok();
            return;
        }

        let setting = args[1].parse::<util::Settings>();
        if setting.is_err() {
            writeln!(io, "Invalid type id").ok();
            return;
        }

        writeln!(io, "setting: {:?}", setting).ok();

        if args.len() == 2 {
            // This Is A Read
            let mut flash = self.flash.lock().await;
            let val = map::fetch_item::<util::Settings, u32, _>(
                flash.deref_mut(),
                FLASH_RANGE,
                &mut NoCache::new(),
                &mut data_buffer,
                &setting.unwrap(),
            )
            .await
            .unwrap();

            writeln!(io, "val = {:?}", val).ok();
            return
        }

        // let's write
        let _result = match setting {
            Ok(util::Settings::CanId) => {
                match args[2].parse::<u32>() {
                    // CAN Address can either be a 11-bit or 29-bit value. We use
                    // the bottom 5 bits as a "command / register" id which lets
                    // us have a total of 32 commands / registers per device on
                    // the bus.
                    //
                    // This means that you end up with either 6-bits or 24-bits for
                    // the node id.
                    Ok(can_id) if can_id < 0xffffff => {
                        // at this point, we know we have a valid value so write it out
                        // to the code
                        let mut flash = self.flash.lock().await;
                        let setting = setting.unwrap();
                        let result = map::store_item(
                            flash.deref_mut(),
                            FLASH_RANGE,
                            &mut NoCache::new(),
                            &mut data_buffer,
                            &setting,
                            &can_id,
                        ).await.map_err(|_x| ParseSettingsError{ });

                        // Publish Notification That CAN ID has changed.
                        let event = ConfigurationEvent::NodeIdUpdate { node_id: can_id };
                        self.publisher.publish(event).await;
                        //self.publisher.publish(setting).await;
                        result
                    },
                    _ => {
                        writeln!(io, "Invalid Can ID").ok();
                        Err(ParseSettingsError {  })
                    }
                }
            },
            Ok(util::Settings::CanReportInterval) => { Ok(()) },
            Err(_) => { Err(ParseSettingsError {  })}
        };

    }

}

pub struct BootloadCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for BootloadCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{
    async fn execute(&self, _args: &[&str], _io: &mut IO) {
        embassy_rp::rom_data::reset_to_usb_boot(0, 0);
    }
}

pub struct RestartCommand;

#[async_trait(?Send)]
impl<IO> CommandHandler<IO> for RestartCommand
where 
    IO: AsyncWrite + FmtWrite + Send
{
    async fn execute(&self, _args: &[&str], _io: &mut IO) {
        cortex_m::peripheral::SCB::sys_reset();
    }
}