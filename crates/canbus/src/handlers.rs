//use crate::can::{CAP, SUBS, PUBS};
extern crate alloc;
use alloc::boxed::Box;
use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::RawMutex;

use crate::can::ConfigurationEvent;
use crate::can_consumer::CanFrameConsumer;

pub struct MyHandler<'a, M: RawMutex, T: Clone, const CAP: usize, const SUBS: usize, const PUBS: usize> {
    configuration_publisher: embassy_sync::pubsub::Publisher<'a, M, T, CAP, SUBS, PUBS> 
}

impl<'a, M: RawMutex, T: Clone, const CAP: usize, const SUBS: usize, const PUBS: usize> MyHandler<'a, M, T, CAP, SUBS, PUBS> {

    pub const fn new(publisher: embassy_sync::pubsub::Publisher<'a, M, T, CAP, SUBS, PUBS>) -> Self {
        Self {
            configuration_publisher: publisher
        }
    }

}

#[async_trait(?Send)]
impl<T: embedded_can::Frame + Sync, M: RawMutex + Sync, const CAP: usize, const SUBS: usize, const PUBS: usize> CanFrameConsumer<T> for MyHandler<'_, M, ConfigurationEvent, CAP, SUBS, PUBS> {

    fn accepts(&self, id: u8, is_remote: bool) -> bool {
        //matches!(id, Id::Standard(sid) if sid.as_raw() >= 0x600 && sid.as_raw() < 0x700)
        id == 0 && is_remote
    }

    async fn on_frame(&mut self, frame: &T) {
        // Handle the frame
        log::info!("got frame: {:?}: {:?}", frame.id(), frame.data());
        // Publish The Rate
        let message = ConfigurationEvent::IntervalUpdate { hz: 10 };
        //self.configuration_publisher.publish_immediate(message);
        self.configuration_publisher.publish(message).await;
    }

    async fn tick(&mut self) {}
}