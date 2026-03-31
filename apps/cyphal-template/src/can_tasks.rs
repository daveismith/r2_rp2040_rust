use crate::can_driver::{Mcp25xxDriver, TimerClock};
use crate::{SpiBusMutex, SpiBusType};
use core::mem::size_of;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use mcp25xx::bitrates::clock_16mhz::CNF_1000K_BPS;
use mcp25xx::registers::{CANINTE, OperationMode, RXB0CTRL, RXB1CTRL, RXM, CANSTAT, EFLG};
use mcp25xx::{AcceptanceFilter, Config, IdHeader, MCP25xx};

use canadensis::Node as _;
use canadensis::core::SubjectId;
use canadensis::core::time::milliseconds;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::core::transport::Transport;
use canadensis::encoding::Deserialize;
use canadensis::node::data_types::{GetInfoResponse, Version};
use canadensis::node::{BasicNode, CoreNode};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::queue::{ArrayQueue, SingleQueueDriver};
use canadensis_can::{CanNodeId, CanReceiver, CanTransferIdTracker, CanTransmitter, Mtu};
use canadensis_data_types::reg::udral::physics::optics::high_color_0_1::HighColor;
use canadensis_data_types::uavcan::node::health_1_0::Health;

const MAX_PUBLISH_TOPICS: usize = 4;
const MAX_REQUEST_SERVICES: usize = 4;
const TX_QUEUE_SIZE: usize = 32;
const LED_COLOR_SUBJECT: SubjectId = SubjectId::from_truncating(5999);
const NODE_ID: u8 = 42;

type SpiDeviceType = SpiDevice<
    'static,
    CriticalSectionRawMutex,
    SpiBusType<'static, peripherals::SPI1>,
    Output<'static>,
>;

type Driver = SingleQueueDriver<TimerClock, ArrayQueue<TX_QUEUE_SIZE>, Mcp25xxDriver<SpiDeviceType>>;

type Node = BasicNode<
    CoreNode<
        TimerClock,
        CanTransmitter<TimerClock, Driver>,
        CanReceiver<TimerClock, Driver>,
        CanTransferIdTracker,
        Driver,
        MAX_PUBLISH_TOPICS,
        MAX_REQUEST_SERVICES,
    >,
>;

fn try_configure_mcp25xx(mcp25xx: &mut MCP25xx<SpiDeviceType>) -> bool {
    let Some(filter_addr) = embedded_can::ExtendedId::new(0) else {
        log::warn!("can: failed to build filter addr");
        return false;
    };
    let Some(filter_mask) = embedded_can::ExtendedId::new(0) else {
        log::warn!("can: failed to build filter mask");
        return false;
    };
    let filters = [
        (AcceptanceFilter::Filter0, IdHeader::from(filter_addr)),
        (AcceptanceFilter::Filter2, IdHeader::from(filter_addr)),
        (AcceptanceFilter::Mask0, IdHeader::from(filter_mask)),
        (AcceptanceFilter::Mask1, IdHeader::from(filter_mask)),
    ];

    let config = Config::default()
        .mode(OperationMode::NormalOperation)
        .bitrate(CNF_1000K_BPS)
        .filters(&filters)
        // Keep filters configured, but disable filter gating on the RX buffers.
        // This guarantees service requests (e.g. uavcan.node.GetInfo) are not dropped.
        .receive_buffer_0(RXB0CTRL::default().with_rxm(RXM::ReceiveAny))
        .receive_buffer_1(RXB1CTRL::default().with_rxm(RXM::ReceiveAny));
    if mcp25xx.apply_config(&config).is_err() {
        log::warn!("can: apply_config failed");
        return false;
    }

    let ints = CANINTE::default().with_rx0ie(true).with_rx1ie(true).with_tx0ie(true);
    if mcp25xx.write_register(ints).is_err() {
        log::warn!("can: write_register failed");
        return false;
    }

    true
}

#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<'static, peripherals::SPI1>,
    cs: Output<'static>,
    mut reset: Output<'static>,
    mut int: Input<'static>,
    unique_id: [u8; 16],
) {
    let spi = SpiDevice::new(spi_bus, cs);
    let mut mcp25xx = MCP25xx { spi };
    let mut init_attempt: u32 = 0;

    loop {
        init_attempt = init_attempt.wrapping_add(1);
        log::info!("can: init attempt {}", init_attempt);
        // Reset transceiver before configuration.
        reset.set_low();
        Timer::after_millis(100).await;
        reset.set_high();
        Timer::after_millis(10).await;

        if !try_configure_mcp25xx(&mut mcp25xx) {
            log::warn!("can: init attempt {} failed", init_attempt);
            Timer::after_millis(250).await;
            continue;
        }
        log::info!("can: mcp configured on attempt {}", init_attempt);

        // Read back CANSTAT and EFLG immediately after config to verify controller state.
        if let Ok(canstat) = mcp25xx.read_register::<CANSTAT>() {
            log::info!("can: CANSTAT opmod={}", canstat.opmod() as u8);
        }
        if let Ok(eflg) = mcp25xx.read_register::<EFLG>() {
            log::info!(
                "can: EFLG txbo={} txep={} rxep={} txwar={} rxwar={}",
                eflg.txbo(), eflg.txep(), eflg.rxep(), eflg.txwar(), eflg.rxwar()
            );
        }
        break;
    }

        let mut node_name = heapless::Vec::new();
        if node_name.extend_from_slice(b"cyphal_template").is_err() {
            log::warn!("can: failed to build node name");
        }

        let node_info = GetInfoResponse {
            protocol_version: Version { major: 1, minor: 0 },
            hardware_version: Version { major: 1, minor: 0 },
            software_version: Version { major: 0, minor: 1 },
            software_vcs_revision_id: 0,
            unique_id,
            name: node_name,
            software_image_crc: Default::default(),
            certificate_of_authenticity: Default::default(),
        };

        let node_id = match CanNodeId::try_from(NODE_ID) {
            Ok(id) => id,
            Err(_) => {
                log::warn!("can: invalid node ID");
                loop {
                    Timer::after_secs(1).await;
                }
            }
        };

        let clock = TimerClock;
        let transmitter = CanTransmitter::new(Mtu::Can8);
        let receiver = CanReceiver::new(node_id);
        let driver = Mcp25xxDriver::new(mcp25xx);
        let queue_driver = SingleQueueDriver::new(ArrayQueue::new(), driver);
        let core = CoreNode::new(clock, node_id, transmitter, receiver, queue_driver);
        let mut node: Node = match BasicNode::new(core, node_info) {
            Ok(node) => node,
            Err(_) => {
                log::warn!("can: BasicNode init failed");
                loop {
                    Timer::after_secs(1).await;
                }
            }
        };

        node.set_health(Health {
            value: Health::NOMINAL,
        });
        if node
            .subscribe_message(LED_COLOR_SUBJECT, size_of::<HighColor>(), milliseconds(10_000))
            .is_err()
        {
            log::warn!("can: subscribe_message failed");
            loop {
                Timer::after_secs(1).await;
            }
        }

        log::info!("can: initialized");

        let mut next_per_second = Instant::now() + Duration::from_secs(1);
        let mut handler = EmptyHandler;
        loop {
            // Wait for CAN activity, but always wake periodically so time-based Cyphal
            // maintenance (heartbeat, transfers) runs even if INT behavior is noisy.
            let _ = with_timeout(Duration::from_millis(20), int.wait_for_low()).await;

            let now = Instant::now();
            while now >= next_per_second {
                let _ = node.run_per_second_tasks();
                // Read EFLG/CANSTAT to detect bus-off or error states.
                if let Some(status) = node.node_mut().driver_mut().driver_mut().read_status()
                {
                    if status.txbo {
                        log::warn!("can: BUS-OFF! Frames not sent. Check bitrate (16MHz crystal?) and termination.");
                    } else if status.txep || status.rxep {
                        log::warn!(
                            "can: error-passive txep={} rxep={} tec={} rec={} tx0if={}",
                            status.txep,
                            status.rxep,
                            status.tec,
                            status.rec,
                            status.tx0if,
                        );
                    } else if status.txwar || status.rxwar {
                        log::warn!(
                            "can: error-warning txwar={} rxwar={} tec={} rec={} tx0if={}",
                            status.txwar,
                            status.rxwar,
                            status.tec,
                            status.rec,
                            status.tx0if,
                        );
                    }

                    if status.txbo || status.txep || status.txwar {
                        log::warn!(
                            "can: txb0 req={} err={} mloa={} abtf={} txif={}",
                            status.txb0.txreq,
                            status.txb0.txerr,
                            status.txb0.mloa,
                            status.txb0.abtf,
                            status.txb0.txif,
                        );
                        if status.txb0.txreq {
                            let recovered = node
                                .node_mut()
                                .driver_mut()
                                .driver_mut()
                                .abort_pending_transmissions();
                            log::warn!("can: abort pending tx buffers recovered={}", recovered);
                        }
                    }
                }
                next_per_second += Duration::from_secs(1);
            }

            // Bound the amount of RX work per executor turn so USB and CLI keep running.
            for _ in 0..16 {
                if node.receive(&mut handler).is_err() {
                    break;
                }
            }
            let _ = node.node_mut().flush();

            // Ensure cooperative scheduling even when INT is held low.
            Timer::after_millis(1).await;
        }
}

struct EmptyHandler;

impl<T: Transport> TransferHandler<T> for EmptyHandler {
    fn handle_message<N>(&mut self, _node: &mut N, transfer: &MessageTransfer<alloc::vec::Vec<u8>, T>) -> bool
    where
        N: canadensis::Node<Transport = T>,
    {
        if transfer.header.subject == LED_COLOR_SUBJECT {
            if let Ok(color) = HighColor::deserialize_from_bytes(&transfer.payload) {
                log::info!(
                    "LED color rx r={} g={} b={}",
                    color.red,
                    color.green,
                    color.blue
                );
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    fn handle_request<N>(
        &mut self,
        _node: &mut N,
        _token: ResponseToken<T>,
        transfer: &ServiceTransfer<alloc::vec::Vec<u8>, T>,
    ) -> bool
    where
        N: canadensis::Node<Transport = T>,
    {
        // Log all service requests (including GetInfo which BasicNode's handler will catch first)
        log::info!(
            "can: service request received service={} client={:?}",
            transfer.header.service, transfer.header.source
        );
        false
    }

    fn handle_response<N>(&mut self, _node: &mut N, _transfer: &ServiceTransfer<alloc::vec::Vec<u8>, T>) -> bool
    where
        N: canadensis::Node<Transport = T>,
    {
        false
    }
}
