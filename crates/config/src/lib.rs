#![no_std]

extern crate alloc;
use alloc::boxed::Box;
use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::pubsub::{publisher::Pub, Publisher, PubSubBehavior};
use dyn_clone::DynClone;

pub trait ConfigEntry: DynClone + Send + Sync {
    fn key(&self) -> &'static str;
}
dyn_clone::clone_trait_object!(ConfigEntry);

pub type BoxedConfigEntry = Box<dyn ConfigEntry + Send + Sync>;

#[async_trait(?Send)]
pub trait ConfigEntryPublisher: Send + Sync {
    async fn publish(&self, ev: BoxedConfigEntry);
}


#[async_trait(?Send)]
impl<'a, PSB> ConfigEntryPublisher for Pub<'a, PSB, BoxedConfigEntry>
where
    PSB: PubSubBehavior<BoxedConfigEntry> + Send + Sync + ?Sized,
{
    async fn publish(&self, _ev: BoxedConfigEntry) {
        //Pub::publish(self, ev).await
    }
}

#[async_trait(?Send)]
impl<'a, R, const N: usize, const NP: usize, const NS: usize>
    ConfigEntryPublisher for Publisher<'a, R, BoxedConfigEntry, N, NP, NS>
where
    R: RawMutex + Send + Sync,
{
    async fn publish(&self, _ev: BoxedConfigEntry) {
        //Publisher::publish(self, ev).await
    }
}