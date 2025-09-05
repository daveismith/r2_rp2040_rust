use alloc::boxed::Box;
use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::pubsub::{publisher::Pub, Publisher, PubSubBehavior};


use crate::can::ConfigurationEvent;

#[async_trait(?Send)]
pub trait ConfigEventPublisher: Send + Sync {
    async fn publish(&self, ev: ConfigurationEvent);
}


#[async_trait(?Send)]
impl<'a, PSB> ConfigEventPublisher for Pub<'a, PSB, ConfigurationEvent>
where
    PSB: PubSubBehavior<ConfigurationEvent> + Send + Sync + ?Sized,
{
    async fn publish(&self, ev: ConfigurationEvent) {
        Pub::publish(self, ev).await
    }
}

#[async_trait(?Send)]
impl<'a, R, const N: usize, const NP: usize, const NS: usize>
    ConfigEventPublisher for Publisher<'a, R, ConfigurationEvent, N, NP, NS>
where
    R: RawMutex + Send + Sync,
{
    async fn publish(&self, ev: ConfigurationEvent) {
        Publisher::publish(self, ev).await
    }
}