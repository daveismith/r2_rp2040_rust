use crate::can_driver::{Mcp25xxDriver, TimerClock};
use crate::FlashMutex;
use crate::{built_info, identify_led};
use crate::{FlashType, SpiBusMutex, SpiBusType};
use core::mem::size_of;
use embassy_boot_rp::{AlignedBuffer, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_embedded_hal::flash::partition::Partition;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{with_timeout, Duration, Instant, Timer, TICK_HZ};
use mcp25xx::bitrates::clock_16mhz::CNF_1000K_BPS;
use mcp25xx::registers::{OperationMode, CANINTE, CANSTAT, EFLG, RXB0CTRL, RXB1CTRL, RXM};
use mcp25xx::{AcceptanceFilter, Config, IdHeader, MCP25xx};

use canadensis::core::time::milliseconds;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::core::Priority;
use canadensis::core::SubjectId;
use canadensis::encoding::Deserialize;
use canadensis::node::data_types::{GetInfoResponse, Version};
use canadensis::node::{BasicNode, CoreNode};
use canadensis::Node as _;
use canadensis::{nb, ResponseToken, ServiceToken, TransferHandler};
use canadensis_can::queue::{ArrayQueue, SingleQueueDriver};
use canadensis_can::{
    CanNodeId, CanReceiver, CanTransferIdTracker, CanTransmitter, CanTransport, Mtu,
};
use canadensis_data_types::reg::udral::physics::optics::high_color_0_1::HighColor;
use canadensis_data_types::uavcan::file::error_1_0::Error as FileError;
use canadensis_data_types::uavcan::file::path_2_0::Path as FilePath;
use canadensis_data_types::uavcan::file::read_1_1::{self, ReadRequest, ReadResponse};
use canadensis_data_types::uavcan::node::execute_command_1_3::{
    ExecuteCommandRequest, ExecuteCommandResponse,
};
use canadensis_data_types::uavcan::node::health_1_0::Health;
use canadensis_data_types::uavcan::node::mode_1_0::Mode;
use canadensis_data_types::uavcan::pnp::node_id_allocation_data_1_0::{
    self as pnp_v1, NodeIDAllocationData as PnpMsg,
};
use core::ops::Range;
use portable_atomic::{AtomicU8, Ordering};

const MAX_PUBLISH_TOPICS: usize = 4;
const MAX_REQUEST_SERVICES: usize = 4;
const TX_QUEUE_SIZE: usize = 32;
const RX_DRAIN_BUDGET: usize = 128;
const LED_COLOR_SUBJECT: SubjectId = SubjectId::from_truncating(5999);
const EXECUTE_COMMAND_PAYLOAD_MAX: usize = 300;
const READ_RESPONSE_PAYLOAD_MAX: usize = 300;
const READ_CHUNK_SIZE: usize = 256;
const OTA_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const OTA_TIMEOUT_RETRY_LIMIT: u8 = 8;
const OTA_PROGRESS_LOG_STEP: usize = 16 * 1024;
const PNP_POLL_TIMEOUT: Duration = Duration::from_millis(20);
const PNP_RETRY_MIN_MS: u64 = 150;
const PNP_RETRY_JITTER_MS: u64 = 850;
const CAN_WAIT_TIMEOUT_IDLE: Duration = Duration::from_millis(20);
const CAN_WAIT_TIMEOUT_OTA: Duration = Duration::from_millis(1);
const LOOP_SLEEP_IDLE: Duration = Duration::from_millis(1);
const LOOP_SLEEP_OTA: Duration = Duration::from_micros(50);

const RESET_NONE: u8 = 0;
const RESET_SOFT: u8 = 1;
const RESET_FACTORY: u8 = 2;

static PENDING_RESET: AtomicU8 = AtomicU8::new(RESET_NONE);
static ASSIGNED_NODE_ID: AtomicU8 = AtomicU8::new(u8::MAX);

pub fn assigned_node_id() -> Option<u8> {
    let node_id = ASSIGNED_NODE_ID.load(Ordering::Acquire);
    if node_id == u8::MAX {
        None
    } else {
        Some(node_id)
    }
}

type FlashPartition<'a> = Partition<'a, CriticalSectionRawMutex, FlashType>;

fn ota_updater_from_linkerfile<'a>(
    dfu_flash: &'a FlashMutex,
    state_flash: &'a FlashMutex,
) -> FirmwareUpdaterConfig<FlashPartition<'a>, FlashPartition<'a>> {
    extern "C" {
        static __bootloader_state_start: u32;
        static __bootloader_state_end: u32;
        static __bootloader_dfu_start: u32;
        static __bootloader_dfu_end: u32;
    }

    let dfu = unsafe {
        let start = &__bootloader_dfu_start as *const u32 as u32;
        let end = &__bootloader_dfu_end as *const u32 as u32;
        Partition::new(dfu_flash, start, end - start)
    };
    let state = unsafe {
        let start = &__bootloader_state_start as *const u32 as u32;
        let end = &__bootloader_state_end as *const u32 as u32;
        Partition::new(state_flash, start, end - start)
    };

    FirmwareUpdaterConfig { dfu, state }
}

struct OtaSession {
    active: bool,
    server_node: CanNodeId,
    path: heapless::Vec<u8, 255>,
    next_offset: usize,
    requests_sent: u32,
    responses_received: u32,
    chunks_written: u32,
    start_tick: u64,
    wait_started_tick: u64,
    total_wait_ticks: u64,
    total_write_ticks: u64,
    next_progress_log_at: usize,
    waiting_response: bool,
    response_deadline: Instant,
    response_timeouts: u8,
    pending_response_payload: Option<heapless::Vec<u8, READ_RESPONSE_PAYLOAD_MAX>>,
}

impl OtaSession {
    fn new() -> Self {
        Self {
            active: false,
            server_node: CanNodeId::MIN,
            path: heapless::Vec::new(),
            next_offset: 0,
            requests_sent: 0,
            responses_received: 0,
            chunks_written: 0,
            start_tick: 0,
            wait_started_tick: 0,
            total_wait_ticks: 0,
            total_write_ticks: 0,
            next_progress_log_at: OTA_PROGRESS_LOG_STEP,
            waiting_response: false,
            response_deadline: Instant::now(),
            response_timeouts: 0,
            pending_response_payload: None,
        }
    }

    fn start(&mut self, server_node: CanNodeId, path_bytes: &[u8]) -> Result<(), ()> {
        if self.active {
            return Err(());
        }
        if path_bytes.is_empty() || path_bytes.len() > FilePath::MAX_LENGTH as usize {
            return Err(());
        }

        let mut path = heapless::Vec::<u8, 255>::new();
        if path.extend_from_slice(path_bytes).is_err() {
            return Err(());
        }

        self.active = true;
        self.server_node = server_node;
        self.path = path;
        self.reset_transfer_state(Instant::now().as_ticks());
        Ok(())
    }

    fn clear(&mut self) {
        self.active = false;
        self.server_node = CanNodeId::MIN;
        self.path.clear();
        self.reset_transfer_state(0);
    }

    fn reset_transfer_state(&mut self, start_tick: u64) {
        self.next_offset = 0;
        self.requests_sent = 0;
        self.responses_received = 0;
        self.chunks_written = 0;
        self.start_tick = start_tick;
        self.wait_started_tick = 0;
        self.total_wait_ticks = 0;
        self.total_write_ticks = 0;
        self.next_progress_log_at = OTA_PROGRESS_LOG_STEP;
        self.waiting_response = false;
        self.response_deadline = Instant::now();
        self.response_timeouts = 0;
        self.pending_response_payload = None;
    }
}

type SpiDeviceType = SpiDevice<
    'static,
    CriticalSectionRawMutex,
    SpiBusType<'static, peripherals::SPI1>,
    Output<'static>,
>;

type Driver =
    SingleQueueDriver<TimerClock, ArrayQueue<TX_QUEUE_SIZE>, Mcp25xxDriver<SpiDeviceType>>;

type Core = CoreNode<
    TimerClock,
    CanTransmitter<TimerClock, Driver>,
    CanReceiver<TimerClock, Driver>,
    CanTransferIdTracker,
    Driver,
    MAX_PUBLISH_TOPICS,
    MAX_REQUEST_SERVICES,
>;

type Node = BasicNode<Core>;

/// Compute the CRC-64/WE hash of the 16-byte unique ID and keep the lowest 48 bits.
/// This matches the canadensis implementation used by the server side.
fn pnp_unique_id_hash(unique_id: &[u8; 16]) -> u64 {
    use crc_any::CRCu64;
    let mut crc = CRCu64::crc64we();
    crc.digest(unique_id);
    crc.get_crc() & 0x0000_ffff_ffff_ffff
}

fn pnp_request_retry_duration(unique_id: &[u8; 16], attempt: u32) -> Duration {
    // A tiny deterministic mixer that gives a per-node jitter in the [min, min+jitter) range.
    let mut state = u32::from_le_bytes([unique_id[0], unique_id[1], unique_id[2], unique_id[3]])
        ^ u32::from_le_bytes([unique_id[4], unique_id[5], unique_id[6], unique_id[7]])
        ^ attempt.wrapping_mul(0x9E37_79B9);
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    let jitter_ms = (state as u64) % PNP_RETRY_JITTER_MS;
    Duration::from_millis(PNP_RETRY_MIN_MS + jitter_ms)
}

/// Receives PnP v1 allocation messages and records the first matching assigned node ID.
struct PnpHandler {
    unique_id_hash: u64,
    /// Set when a matching allocation response arrives.
    assigned_id: Option<CanNodeId>,
}

impl PnpHandler {
    fn new(unique_id: &[u8; 16]) -> Self {
        Self {
            unique_id_hash: pnp_unique_id_hash(unique_id),
            assigned_id: None,
        }
    }
}

impl TransferHandler<CanTransport> for PnpHandler {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        transfer: &canadensis::core::transfer::MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.subject != pnp_v1::SUBJECT {
            return false;
        }
        // Rule C: restart our timer on any allocation message (handled externally).
        // Rule D: accept only non-anonymous sources that match our hash and carry a node ID.
        if transfer.header.source.is_none() {
            // Anonymous source means it's a request from another allocatee, not a response.
            return true;
        }
        let Ok(msg) = PnpMsg::deserialize_from_bytes(&transfer.payload) else {
            return true;
        };
        if msg.unique_id_hash != self.unique_id_hash {
            return true;
        }
        if let Some(id_entry) = msg.allocated_node_id.iter().next() {
            if let Ok(node_id) = CanNodeId::try_from(id_entry.value as u8) {
                self.assigned_id = Some(node_id);
            }
        }
        true
    }
}

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

    let ints = CANINTE::default()
        .with_rx0ie(true)
        .with_rx1ie(true)
        .with_tx0ie(true);
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
    flash: &'static FlashMutex,
    nvs_range: Range<u32>,
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
                eflg.txbo(),
                eflg.txep(),
                eflg.rxep(),
                eflg.txwar(),
                eflg.rxwar()
            );
        }
        break;
    }

    let mut node_name = heapless::Vec::new();
    if node_name
        .extend_from_slice(built_info::PKG_NAME.as_bytes())
        .is_err()
    {
        log::warn!("can: failed to build node name");
    }

    let software_major = u8::from_str_radix(built_info::PKG_VERSION_MAJOR, 10).unwrap_or(0);
    let software_minor = u8::from_str_radix(built_info::PKG_VERSION_MINOR, 10).unwrap_or(0);
    let software_vcs_revision_id = built_info::GIT_COMMIT_HASH_SHORT
        .and_then(|hash| u64::from_str_radix(hash, 16).ok())
        .unwrap_or(0);

    let node_info = GetInfoResponse {
        protocol_version: Version { major: 1, minor: 0 },
        hardware_version: Version { major: 1, minor: 0 },
        software_version: Version {
            major: software_major,
            minor: software_minor,
        },
        software_vcs_revision_id,
        unique_id,
        name: node_name,
        software_image_crc: Default::default(),
        certificate_of_authenticity: Default::default(),
    };

    let clock = TimerClock;
    let transmitter = CanTransmitter::new(Mtu::Can8);
    let receiver = CanReceiver::new_anonymous();
    let driver = Mcp25xxDriver::new(mcp25xx);
    let queue_driver = SingleQueueDriver::new(ArrayQueue::new(), driver);
    let mut core: Core = CoreNode::new_anonymous(clock, transmitter, receiver, queue_driver);

    // Phase A: subscribe to allocation messages (budget = 9 bytes for the full response).
    // Start publishing on the same subject; anonymous node, so these are anonymous transfers.
    if core
        .subscribe_message(pnp_v1::SUBJECT, 9, milliseconds(1_000))
        .is_err()
    {
        log::warn!("can: pnp: subscribe_message failed");
        loop {
            Timer::after_secs(1).await;
        }
    }
    if core
        .start_publishing(
            pnp_v1::SUBJECT,
            milliseconds(1_000),
            canadensis::core::Priority::Nominal.into(),
        )
        .is_err()
    {
        log::warn!("can: pnp: start_publishing failed");
        loop {
            Timer::after_secs(1).await;
        }
    }

    let pnp_request = PnpMsg {
        unique_id_hash: pnp_unique_id_hash(&unique_id),
        allocated_node_id: heapless::Vec::new(), // empty = request, per spec
    };
    let mut pnp_handler = PnpHandler::new(&unique_id);
    let mut pnp_requests_sent: u32 = 0;
    let mut next_pnp_request_at = Instant::now();
    log::info!("can: waiting for dynamic node ID allocation (pnp v1)");
    let node_id = loop {
        let _ = with_timeout(PNP_POLL_TIMEOUT, int.wait_for_low()).await;

        for _ in 0..RX_DRAIN_BUDGET {
            if core.receive(&mut pnp_handler).is_err() {
                break;
            }
        }

        if let Some(id) = pnp_handler.assigned_id {
            break id;
        }

        let now = Instant::now();
        if now >= next_pnp_request_at {
            match core.publish(pnp_v1::SUBJECT, &pnp_request) {
                Ok(()) => {
                    pnp_requests_sent = pnp_requests_sent.wrapping_add(1);
                    log::info!("can: pnp allocation request {} sent", pnp_requests_sent);
                    let _ = core.flush();
                }
                Err(nb::Error::WouldBlock) => {
                    // TX queue temporarily full; will retry at next interval.
                }
                Err(nb::Error::Other(err)) => {
                    log::warn!("can: pnp allocation request failed: {:?}", err);
                }
            }
            next_pnp_request_at = now + pnp_request_retry_duration(&unique_id, pnp_requests_sent);
        }

        Timer::after(LOOP_SLEEP_IDLE).await;
    };
    log::info!(
        "can: allocated node ID {} after {} request(s)",
        node_id.to_u8(),
        pnp_requests_sent
    );
    ASSIGNED_NODE_ID.store(node_id.to_u8(), Ordering::Release);

    // Promote the anonymous CoreNode to a named node before handing it to BasicNode.
    use canadensis::Node as NodeTrait;
    NodeTrait::set_node_id(&mut core, node_id);

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
        .subscribe_request(
            canadensis_data_types::uavcan::node::execute_command_1_3::SERVICE,
            EXECUTE_COMMAND_PAYLOAD_MAX,
            milliseconds(2_000),
        )
        .is_err()
    {
        log::warn!("can: subscribe_request execute_command failed");
        loop {
            Timer::after_secs(1).await;
        }
    }

    if node
        .subscribe_message(
            LED_COLOR_SUBJECT,
            size_of::<HighColor>(),
            milliseconds(10_000),
        )
        .is_err()
    {
        log::warn!("can: subscribe_message failed");
        loop {
            Timer::after_secs(1).await;
        }
    }

    let updater_config = ota_updater_from_linkerfile(flash, flash);
    let mut ota_aligned = AlignedBuffer([0; 4]);
    let mut updater = FirmwareUpdater::new(updater_config, &mut ota_aligned.0);
    let mark_boot = match updater.get_state().await {
        Ok(State::Revert) => {
            log::info!("boot state: revert, marking booted");
            true
        }
        Ok(state) => {
            log::info!("boot state: {:?}", state);
            true
        }
        Err(err) => {
            log::warn!("boot state read failed: {:?}", err);
            false
        }
    };

    if mark_boot {
        if let Err(err) = updater.mark_booted().await {
            log::warn!("boot state mark_booted failed: {:?}", err);
        }
    }

    let read_service = match node.start_sending_requests::<ReadRequest>(
        read_1_1::SERVICE,
        milliseconds(2_000),
        READ_RESPONSE_PAYLOAD_MAX,
        Priority::Nominal,
    ) {
        Ok(token) => token,
        Err(err) => {
            log::warn!("can: start_sending_requests file.read failed: {:?}", err);
            loop {
                Timer::after_secs(1).await;
            }
        }
    };

    log::info!("can: initialized");

    let mut next_per_second = Instant::now() + Duration::from_secs(1);
    let mut handler = AppHandler::new();
    loop {
        let reset_mode = PENDING_RESET.swap(RESET_NONE, Ordering::AcqRel);
        if reset_mode != RESET_NONE {
            // The ExecuteCommand response may span multiple CAN frames
            // (e.g. "factory_reset" → 3 frames at 7 B/frame). The initial
            // flush() at the bottom of the loop put frame-1 into TXB0.
            // We need two more flush+wait cycles so frames 2 and 3 reach
            // the bus before any long blocking operation.
            for _ in 0..2 {
                let _ = node.node_mut().flush();
                Timer::after_millis(100).await;
            }

            if reset_mode == RESET_FACTORY {
                let result = {
                    let mut flash = flash.lock().await;
                    flash.blocking_erase(nvs_range.start, nvs_range.end)
                };
                if let Err(err) = result {
                    log::error!("factory reset erase failed: {:?}", err);
                } else {
                    log::info!("factory reset erased NVS region");
                }
            }

            let _ = node.node_mut().flush();
            Timer::after_millis(100).await;
            cortex_m::peripheral::SCB::sys_reset();
        }

        // Wait for CAN activity, but always wake periodically so time-based Cyphal
        // maintenance (heartbeat, transfers) runs even if INT behavior is noisy.
        // Use shorter waits during OTA to reduce request-to-request latency.
        let can_wait_timeout = if handler.ota.active {
            CAN_WAIT_TIMEOUT_OTA
        } else {
            CAN_WAIT_TIMEOUT_IDLE
        };
        let _ = with_timeout(can_wait_timeout, int.wait_for_low()).await;

        let now = Instant::now();
        while now >= next_per_second {
            let _ = node.run_per_second_tasks();
            // Read EFLG/CANSTAT to detect bus-off or error states.
            if let Some(status) = node.node_mut().driver_mut().driver_mut().read_status() {
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

        // Drain more frames per turn so large multi-frame service transfers
        // (e.g. file.Read responses during OTA) don't overflow MCP25xx RX buffers.
        for _ in 0..RX_DRAIN_BUDGET {
            if node.receive(&mut handler).is_err() {
                break;
            }
        }

        handler
            .drive_ota(&mut node, &read_service, &mut updater)
            .await;
        if handler.ota.active {
            node.set_mode(Mode {
                value: Mode::SOFTWARE_UPDATE,
            });
        } else {
            node.set_mode(Mode {
                value: Mode::OPERATIONAL,
            });
        }

        let _ = node.node_mut().flush();

        // Ensure cooperative scheduling even when INT is held low.
        let loop_sleep = if handler.ota.active {
            LOOP_SLEEP_OTA
        } else {
            LOOP_SLEEP_IDLE
        };
        Timer::after(loop_sleep).await;
    }
}

struct AppHandler {
    ota: OtaSession,
}

impl AppHandler {
    fn new() -> Self {
        Self {
            ota: OtaSession::new(),
        }
    }

    fn abort_ota(&mut self, reason: &str) {
        log::warn!(
            "ota: abort reason='{}' downloaded={} req={} resp={}",
            reason,
            self.ota.next_offset,
            self.ota.requests_sent,
            self.ota.responses_received
        );
        self.ota.clear();
    }

    fn accumulate_wait_ticks(&mut self, now_ticks: u64) {
        if self.ota.wait_started_tick != 0 {
            self.ota.total_wait_ticks = self
                .ota
                .total_wait_ticks
                .saturating_add(now_ticks.saturating_sub(self.ota.wait_started_tick));
        }
        self.ota.wait_started_tick = 0;
    }

    fn ticks_to_ms(ticks: u64) -> u64 {
        if TICK_HZ > 0 {
            ticks.saturating_mul(1000) / TICK_HZ
        } else {
            0
        }
    }
}

impl TransferHandler<CanTransport> for AppHandler {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
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
        node: &mut N,
        token: ResponseToken<CanTransport>,
        transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.service
            != canadensis_data_types::uavcan::node::execute_command_1_3::SERVICE
        {
            return false;
        }

        let (status, output) =
            match ExecuteCommandRequest::deserialize_from_bytes(&transfer.payload) {
                Ok(request) => self.handle_execute_command(&request, transfer.header.source),
                Err(_) => (
                    ExecuteCommandResponse::STATUS_BAD_PARAMETER,
                    b"invalid request".as_slice(),
                ),
            };

        let mut response_output = heapless::Vec::<u8, 46>::new();
        let _ = response_output.extend_from_slice(output);
        let response = ExecuteCommandResponse {
            status,
            output: response_output,
        };

        if let Err(err) = node.send_response(token, milliseconds(1000), &response) {
            log::warn!("can: failed to send ExecuteCommand response: {:?}", err);
        }

        true
    }

    fn handle_response<N>(
        &mut self,
        _node: &mut N,
        transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        if transfer.header.service != read_1_1::SERVICE {
            return false;
        }
        if !self.ota.active || !self.ota.waiting_response {
            return false;
        }
        if transfer.header.source != self.ota.server_node {
            return false;
        }

        let mut payload = heapless::Vec::<u8, READ_RESPONSE_PAYLOAD_MAX>::new();
        if payload.extend_from_slice(&transfer.payload).is_err() {
            log::warn!("ota: file.read response too large");
            return true;
        }
        self.ota.pending_response_payload = Some(payload);
        true
    }
}

impl AppHandler {
    fn handle_execute_command(
        &mut self,
        request: &ExecuteCommandRequest,
        source_node: CanNodeId,
    ) -> (u8, &'static [u8]) {
        match request.command {
            ExecuteCommandRequest::COMMAND_IDENTIFY => {
                log::info!("ExecuteCommand: IDENTIFY");
                identify_led::trigger_identify(Duration::from_secs(5));
                (ExecuteCommandResponse::STATUS_SUCCESS, b"identify")
            }
            ExecuteCommandRequest::COMMAND_RESTART => {
                log::info!("ExecuteCommand: RESTART");
                PENDING_RESET.store(RESET_SOFT, Ordering::Release);
                (ExecuteCommandResponse::STATUS_SUCCESS, b"restart")
            }
            ExecuteCommandRequest::COMMAND_FACTORY_RESET => {
                log::info!("ExecuteCommand: FACTORY_RESET");
                PENDING_RESET.store(RESET_FACTORY, Ordering::Release);
                (ExecuteCommandResponse::STATUS_SUCCESS, b"factory_reset")
            }
            ExecuteCommandRequest::COMMAND_BEGIN_SOFTWARE_UPDATE => {
                if request.parameter.is_empty()
                    || request.parameter.len() > FilePath::MAX_LENGTH as usize
                {
                    (ExecuteCommandResponse::STATUS_BAD_PARAMETER, b"bad path")
                } else if self.ota.active {
                    (ExecuteCommandResponse::STATUS_BAD_STATE, b"ota busy")
                } else {
                    if self.ota.start(source_node, &request.parameter).is_err() {
                        (ExecuteCommandResponse::STATUS_BAD_PARAMETER, b"bad path")
                    } else {
                        let path_len = request.parameter.len();
                        let path_preview =
                            core::str::from_utf8(&request.parameter).unwrap_or("<non-utf8-path>");
                        log::info!(
                            "ota begin: server={} path_len={} path='{}'",
                            source_node.to_u8(),
                            path_len,
                            path_preview
                        );
                        (ExecuteCommandResponse::STATUS_SUCCESS, b"ota")
                    }
                }
            }
            _ => (ExecuteCommandResponse::STATUS_BAD_COMMAND, b"unsupported"),
        }
    }

    async fn drive_ota(
        &mut self,
        node: &mut Node,
        read_service: &ServiceToken<ReadRequest>,
        updater: &mut FirmwareUpdater<'_, FlashPartition<'_>, FlashPartition<'_>>,
    ) {
        if !self.ota.active {
            return;
        }

        if self.ota.waiting_response {
            if let Some(payload) = self.ota.pending_response_payload.take() {
                self.accumulate_wait_ticks(Instant::now().as_ticks());
                self.ota.waiting_response = false;
                self.ota.response_timeouts = 0;
                self.ota.responses_received = self.ota.responses_received.saturating_add(1);

                let response = match ReadResponse::deserialize_from_bytes(&payload) {
                    Ok(response) => response,
                    Err(_) => {
                        self.abort_ota("invalid file.read response");
                        return;
                    }
                };

                let read_error = response.error.value;
                if read_error != FileError::OK {
                    log::warn!("ota: file.read error={}", read_error);
                    self.abort_ota("file.read returned non-OK error");
                    return;
                }

                let chunk = response.data.value;
                let chunk_len = chunk.len();
                if chunk_len > 0 {
                    let write_start_tick = Instant::now().as_ticks();
                    if updater
                        .write_firmware(self.ota.next_offset, &chunk)
                        .await
                        .is_err()
                    {
                        self.abort_ota("flash write failed");
                        return;
                    }
                    self.ota.total_write_ticks = self
                        .ota
                        .total_write_ticks
                        .saturating_add(Instant::now().as_ticks().saturating_sub(write_start_tick));
                    self.ota.chunks_written = self.ota.chunks_written.saturating_add(1);
                    self.ota.next_offset += chunk_len;
                    if self.ota.next_offset >= self.ota.next_progress_log_at {
                        log::info!(
                            "ota: progress downloaded={}B req={} resp={}",
                            self.ota.next_offset,
                            self.ota.requests_sent,
                            self.ota.responses_received
                        );
                        self.ota.next_progress_log_at += OTA_PROGRESS_LOG_STEP;
                    }
                }

                if chunk_len < READ_CHUNK_SIZE {
                    let elapsed_ticks = Instant::now()
                        .as_ticks()
                        .saturating_sub(self.ota.start_tick);
                    let elapsed_ms = Self::ticks_to_ms(elapsed_ticks);
                    let effective_bitrate_bps = if elapsed_ticks > 0 {
                        (self.ota.next_offset as u64)
                            .saturating_mul(8)
                            .saturating_mul(TICK_HZ)
                            / elapsed_ticks
                    } else {
                        0
                    };
                    let effective_bitrate_kbps = effective_bitrate_bps / 1000;
                    let wait_ms = Self::ticks_to_ms(self.ota.total_wait_ticks);
                    let write_ms = Self::ticks_to_ms(self.ota.total_write_ticks);
                    let other_ms = elapsed_ms.saturating_sub(wait_ms.saturating_add(write_ms));
                    let wait_pct = if elapsed_ticks > 0 {
                        self.ota.total_wait_ticks.saturating_mul(100) / elapsed_ticks
                    } else {
                        0
                    };
                    let write_pct = if elapsed_ticks > 0 {
                        self.ota.total_write_ticks.saturating_mul(100) / elapsed_ticks
                    } else {
                        0
                    };
                    let avg_chunk_bytes = if self.ota.chunks_written > 0 {
                        self.ota.next_offset / self.ota.chunks_written as usize
                    } else {
                        0
                    };

                    log::info!(
                        "ota: transfer complete bytes={} time_ms={} bitrate={} bps ({} kbps) req={} resp={} chunks={} avg_chunk={}B",
                        self.ota.next_offset,
                        elapsed_ms,
                        effective_bitrate_bps,
                        effective_bitrate_kbps,
                        self.ota.requests_sent,
                        self.ota.responses_received,
                        self.ota.chunks_written,
                        avg_chunk_bytes
                    );
                    log::info!(
                        "ota: timing breakdown wait_ms={} ({}%) write_ms={} ({}%) other_ms={}",
                        wait_ms,
                        wait_pct,
                        write_ms,
                        write_pct,
                        other_ms
                    );
                    match updater.mark_updated().await {
                        Ok(()) => {
                            log::info!("ota: marked updated, rebooting");
                            self.ota.clear();
                            PENDING_RESET.store(RESET_SOFT, Ordering::Release);
                        }
                        Err(err) => {
                            log::warn!("ota: mark_updated failed: {:?}", err);
                            self.abort_ota("mark_updated failed");
                        }
                    }
                }
            } else if Instant::now() >= self.ota.response_deadline {
                self.accumulate_wait_ticks(Instant::now().as_ticks());
                self.ota.waiting_response = false;
                self.ota.response_timeouts = self.ota.response_timeouts.saturating_add(1);
                if self.ota.response_timeouts > OTA_TIMEOUT_RETRY_LIMIT {
                    self.abort_ota("file.read response timeout");
                } else {
                    log::warn!(
                        "ota: file.read response timeout at offset={} retry={}/{}",
                        self.ota.next_offset,
                        self.ota.response_timeouts,
                        OTA_TIMEOUT_RETRY_LIMIT
                    );
                }
                return;
            } else {
                return;
            }
        }

        if !self.ota.active {
            return;
        }

        let request = ReadRequest {
            offset: self.ota.next_offset as u64,
            path: FilePath {
                path: self.ota.path.clone(),
            },
        };
        match node.send_request(read_service, &request, self.ota.server_node) {
            Ok(_) => {
                self.ota.requests_sent = self.ota.requests_sent.saturating_add(1);
                self.ota.waiting_response = true;
                self.ota.wait_started_tick = Instant::now().as_ticks();
                self.ota.response_deadline = Instant::now() + OTA_RESPONSE_TIMEOUT;
                // Push request frames out immediately to minimize request/response latency.
                let _ = node.node_mut().flush();
            }
            Err(nb::Error::WouldBlock) => {
                // Try again next loop.
            }
            Err(nb::Error::Other(_)) => {
                self.abort_ota("failed to send file.read request");
            }
        }
    }
}
