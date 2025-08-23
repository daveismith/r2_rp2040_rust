use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use embedded_io_async::Write as AsyncWrite;
use core::ops::DerefMut;

use crate::cli::CommandHandler;

use sequential_storage::cache::NoCache;
use sequential_storage::map;

use crate::util::ParseSettingsError;
use crate::util;
use crate::{FlashMutex, FLASH_RANGE};

use crate::can::{ConfigurationEvent, CONFIGURATION_CHANNEL, ConfigurationEventPublisherType};


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