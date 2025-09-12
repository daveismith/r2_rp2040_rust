use crate::FlashMutex;
use crate::can::{ConfigurationEvent, CONFIGURATION_CHANNEL, ConfigurationEventPublisherType};
use crate::util::ParseSettingsError;
use crate::util;

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Write as FmtWrite;
use core::ops::{DerefMut, Range};
use embedded_io_async::Write as AsyncWrite;
use sequential_storage::cache::NoCache;
use sequential_storage::map;
use usb_cli::CommandHandler;

pub struct CanCommand<'a> {
    pub flash: &'a FlashMutex,
    pub flash_range: &'a Range<u32>,
    pub publisher: ConfigurationEventPublisherType<'a>
}

//TODO: Figure Out How To Manage This
impl<'a> CanCommand<'a> {

    pub fn new(
        flash: &'a FlashMutex,
        flash_range: &'a Range<u32>,
    ) -> Self {

        Self { 
            flash: flash,
            flash_range: flash_range,
            publisher: CONFIGURATION_CHANNEL.publisher().unwrap(),
        }
    }

}

#[async_trait(?Send)]
impl<'a, IO> CommandHandler<IO> for CanCommand<'a>
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
                self.flash_range.clone(),
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
                            self.flash_range.clone(),
                            &mut NoCache::new(),
                            &mut data_buffer,
                            &setting,
                            &can_id,
                        ).await.map_err(|_x| ParseSettingsError{ });

                        // Publish Notification That CAN ID has changed.
                        let event = ConfigurationEvent::NodeIdUpdate { node_id: can_id };
                        self.publisher.publish(event).await;
                        result
                    },
                    _ => {
                        writeln!(io, "Invalid Can ID").ok();
                        Err(ParseSettingsError {  })
                    }
                }
            },
            Ok(util::Settings::CanReportInterval) => { 
                match args[2].parse::<u64>() {
                    Ok(hz) => {
                        let event = ConfigurationEvent::IntervalUpdate { hz: hz };
                        self.publisher.publish(event).await;
                        Ok(())
                    },
                    _ => {
                        writeln!(io, "Invalid Interval").ok();
                        Err(ParseSettingsError { })
                    }
                }
            },
            Err(_) => { Err(ParseSettingsError {  })}
        };

    }

}