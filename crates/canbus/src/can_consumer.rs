use embedded_can;
use embedded_can::Id;
use heapless::Vec;
use async_trait::async_trait;

extern crate alloc;
use alloc::boxed::Box;

use embedded_can::Frame as CanFrameTrait;

#[async_trait(?Send)]
pub trait CanFrameConsumer<F: CanFrameTrait> {
    fn set_node_id(&mut self, _node_id: u32) {}
    fn accepts(&self, id: u8, is_remote: bool) -> bool;
    async fn on_frame(&mut self, frame: &F);
    async fn tick(&mut self);
}

/// Dispatcher for routing frames to registered consumers
pub struct CanSimpleDispatcher<'a, const N: usize, F: CanFrameTrait> {
    node_id: u32,
    consumers: Vec<&'a mut (dyn CanFrameConsumer<F>), N>,
}

impl<'a, const N: usize, F: CanFrameTrait> CanSimpleDispatcher<'a, N, F> {
    pub const fn new(node_id: u32) -> Self {
        Self {
            node_id: node_id,
            consumers: Vec::new(),
        }
    }

    pub fn set_node_id(&mut self, node_id: u32) {
        self.node_id = node_id;

        // Update all the consumers
        for consumer in self.consumers.iter_mut() {
            consumer.set_node_id(node_id);
        }
    }

    pub fn register(&mut self, consumer: &'a mut (dyn CanFrameConsumer<F> + 'a)) -> Result<(), ()> {
        self.consumers.push(consumer).map_err(|_| ())
    }

    pub async fn dispatch(&mut self, frame: &F) {

        // Extract The Node Id
        let (node_id, cmd_id) = match frame.id() {
            Id::Standard(simple) => {
                let id = simple.as_raw();
                ((id >> 5) as u32, (id & 0x1f) as u8)
            },
            Id::Extended(extended) => {
                let id = extended.as_raw();
                (id >> 5, (id & 0x1f) as u8)
            }
        };

        // Check if this is a message for us
        if node_id != self.node_id {
            log::info!("Incorrect Node Id: {:?}", node_id);
            return;
        }
        
        // Process The Frame
        let mut handled = false;
        for consumer in self.consumers.iter_mut() {
            if consumer.accepts(cmd_id, frame.is_remote_frame()) {
                consumer.on_frame(frame).await;
                handled = true;
                break;
            }
        }
        if !handled {
            log::info!("Unhandled CAN Frame: [id: {:?}, rtr: {:?}], data: {:?}", frame.id(), frame.is_remote_frame(), frame.data());
        }
    }

    pub async fn tick(&mut self) {
        for consumer in self.consumers.iter_mut() {
            consumer.tick().await;
        }
    }
}