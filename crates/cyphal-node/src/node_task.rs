//! Concrete RP2040 Cyphal node task with MCP25xx CAN driver.
//!
//! Provides [`run_cyphal_node`] — an async function that manages the complete
//! Cyphal node lifecycle:
//!
//! 1. MCP25xx hardware initialisation (with retry)
//! 2. PnP v1 dynamic node-ID allocation
//! 3. Node promotion to named [`canadensis::node::BasicNode`]
//! 4. Service subscriptions (ExecuteCommand, file.Read)
//! 5. Embassy-bootloader mark-booted handshake
//! 6. Main loop (receive → OTA drive → health/mode update → flush)
//!
//! # Usage
//! Wrap `run_cyphal_node` in your own `#[embassy_executor::task]`:
//!
//! ```rust,ignore
//! #[embassy_executor::task]
//! async fn can_handler(
//!     spi_bus: &'static SpiBusMutex<peripherals::SPI1>,
//!     cs: Output<'static>,
//!     reset: Output<'static>,
//!     int: Input<'static>,
//!     flash: &'static FlashMutex,
//!     unique_id: [u8; 16],
//! ) {
//!     let node_info = NodeInfoConfig::new(unique_id)
//!         .build_with_env(
//!             env!("CARGO_PKG_NAME"),
//!             env!("CARGO_PKG_VERSION_MAJOR"),
//!             env!("CARGO_PKG_VERSION_MINOR"),
//!         );
//!     let command_handler = DefaultCommandHandler::new(
//!         Some(my_identify_fn),
//!         NVS_RANGE,
//!     );
//!     let handler = CyphalHandler::new(command_handler, NoopHandler);
//!     cyphal_node::run_cyphal_node(
//!         spi_bus, cs, reset, int, flash, NVS_RANGE, unique_id, node_info, handler,
//!     )
//!     .await;
//! }
//! ```

use core::cell::RefCell;
use core::ops::Range;

use canadensis::core::time::milliseconds;
use canadensis::core::Priority;
use canadensis::Node as NodeTrait;
use canadensis::{nb, TransferHandler};
use canadensis::node::{BasicNode, CoreNode};
use canadensis::node::data_types::GetInfoResponse;
use canadensis_can::queue::{ArrayQueue, SingleQueueDriver};
use canadensis_can::{
    CanReceiver, CanTransferIdTracker, CanTransmitter, CanTransport, Mtu,
};
use canadensis_data_types::uavcan::file::path_2_0::Path as FilePath;
use canadensis_data_types::uavcan::file::read_1_1::{ReadRequest, ReadResponse};
use canadensis_data_types::uavcan::file::error_1_0::Error as FileError;
use canadensis_data_types::uavcan::node::health_1_0::Health;
use canadensis_data_types::uavcan::node::mode_1_0::Mode;
use canadensis_data_types::uavcan::pnp::node_id_allocation_data_1_0 as pnp_v1;
use canadensis_data_types::uavcan::pnp::node_id_allocation_data_1_0::NodeIDAllocationData as PnpMsg;
use canadensis::encoding::Deserialize;
use embassy_boot_rp::{AlignedBuffer, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_embedded_hal::flash::partition::Partition;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_rp::flash;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;
use embassy_rp::spi::{self, Spi};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_time::{with_timeout, Duration, Instant, Timer, TICK_HZ};
use mcp25xx::bitrates::clock_16mhz::CNF_1000K_BPS;
use mcp25xx::registers::{OperationMode, CANINTE, CANSTAT, EFLG, RXB0CTRL, RXB1CTRL, RXM};
use mcp25xx::{AcceptanceFilter, Config, IdHeader, MCP25xx};
use portable_atomic::{AtomicU8, Ordering};

use crate::clock::TimerClock;
use crate::driver::Mcp25xxDriver;
use crate::execute_command::{PENDING_RESET, RESET_FACTORY, RESET_NONE, RESET_SOFT};
use crate::handler::{CyphalHandler, NodeExtension};
use crate::ota::{
    OTA_PROGRESS_LOG_STEP, OTA_RESPONSE_TIMEOUT, OTA_TIMEOUT_RETRY_LIMIT, READ_CHUNK_SIZE,
    READ_RESPONSE_PAYLOAD_MAX,
};
use crate::pnp::{
    pnp_request_retry_duration, pnp_unique_id_hash, PnpHandler, LOOP_SLEEP_IDLE,
    PNP_POLL_TIMEOUT,
};

// ---- Flash size -----------------------------------------------------------

const FLASH_SIZE: usize = 8 * 1024 * 1024;

// ---- Public type aliases --------------------------------------------------

/// RP2040 flash peripheral type used with the Embassy bootloader.
pub type FlashType =
    embassy_rp::flash::Flash<'static, peripherals::FLASH, flash::Async, FLASH_SIZE>;

/// Async mutex wrapping the shared flash peripheral.
pub type FlashMutex =
    embassy_sync::mutex::Mutex<CriticalSectionRawMutex, FlashType>;

/// Blocking SPI bus type (peripheral is generic over the SPI instance `T`).
pub type SpiBusType<T> = Spi<'static, T, spi::Blocking>;

/// Blocking mutex wrapping the shared SPI bus.
pub type SpiBusMutex<T> =
    BlockingMutex<CriticalSectionRawMutex, RefCell<SpiBusType<T>>>;

// ---- Internal type aliases ------------------------------------------------

type SpiDeviceType<T> = SpiDevice<
    'static,
    CriticalSectionRawMutex,
    SpiBusType<T>,
    Output<'static>,
>;

type Driver<T> =
    SingleQueueDriver<TimerClock, ArrayQueue<TX_QUEUE_SIZE>, Mcp25xxDriver<SpiDeviceType<T>>>;

type Core<T> = CoreNode<
    TimerClock,
    CanTransmitter<TimerClock, Driver<T>>,
    CanReceiver<TimerClock, Driver<T>>,
    CanTransferIdTracker,
    Driver<T>,
    MAX_PUBLISH_TOPICS,
    MAX_REQUEST_SERVICES,
>;

type Node<T> = BasicNode<Core<T>>;

type FlashPartition<'a> = Partition<'a, CriticalSectionRawMutex, FlashType>;

// ---- Constants ------------------------------------------------------------

const MAX_PUBLISH_TOPICS: usize = 4;
const MAX_REQUEST_SERVICES: usize = 4;
const TX_QUEUE_SIZE: usize = 32;
const RX_DRAIN_BUDGET: usize = 128;
const RX_DRAIN_EXTRA_ROUNDS_OTA: usize = 4;
const EXECUTE_COMMAND_PAYLOAD_MAX: usize = 300;
const CAN_WAIT_TIMEOUT_IDLE: Duration = Duration::from_millis(20);
const CAN_WAIT_TIMEOUT_OTA: Duration = Duration::from_micros(100);
const LOOP_SLEEP_OTA: Duration = Duration::from_micros(20);

// ---- Global state ---------------------------------------------------------

/// The node ID assigned by the PnP allocator.
///
/// Contains `u8::MAX` until PnP allocation completes.
pub static ASSIGNED_NODE_ID: AtomicU8 = AtomicU8::new(u8::MAX);

/// Return the assigned Cyphal node ID, or `None` if PnP allocation has not
/// yet completed.
pub fn assigned_node_id() -> Option<u8> {
    let id = ASSIGNED_NODE_ID.load(Ordering::Acquire);
    if id == u8::MAX { None } else { Some(id) }
}

// ---- MCP25xx configuration ------------------------------------------------

fn try_configure_mcp25xx<T: embassy_rp::spi::Instance>(
    mcp25xx: &mut MCP25xx<SpiDeviceType<T>>,
) -> bool {
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
        // Disable filter gating so service requests (e.g. GetInfo) are never dropped.
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

// ---- Linker-file flash partition helper -----------------------------------

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

// ---- OTA drive step -------------------------------------------------------

/// Drive one iteration of the OTA state machine.
///
/// Sends `file.Read` requests, processes received responses, writes firmware
/// chunks, and completes the update when the last chunk arrives.
///
/// Called from the main loop in [`run_cyphal_node`] after each receive phase.
/// The caller flushes the TX queue immediately after this returns.
async fn drive_ota_step<T, E>(
    handler: &mut CyphalHandler<E>,
    node: &mut Node<T>,
    read_service: &canadensis::ServiceToken<ReadRequest>,
    updater: &mut FirmwareUpdater<'_, FlashPartition<'_>, FlashPartition<'_>>,
) where
    T: embassy_rp::spi::Instance,
    E: TransferHandler<CanTransport>,
{
    let ota = &mut handler.ota;
    if !ota.active {
        return;
    }

    let now_ticks = Instant::now().as_ticks();

    if ota.waiting_response {
        if let Some(payload) = ota.pending_response_payload.take() {
            ota.accumulate_wait_ticks(now_ticks);
            ota.waiting_response = false;
            ota.response_timeouts = 0;
            ota.responses_received = ota.responses_received.saturating_add(1);

            let response = match ReadResponse::deserialize_from_bytes(&payload) {
                Ok(r) => r,
                Err(_) => {
                    ota.abort("invalid file.read response");
                    return;
                }
            };

            let error_value = { response.error.value };
            if error_value != FileError::OK {
                log::warn!("ota: file.read error={}", error_value);
                ota.abort("file.read returned non-OK error");
                return;
            }

            let chunk = response.data.value;
            let chunk_len = chunk.len();
            if chunk_len > 0 {
                let write_start_tick = Instant::now().as_ticks();
                if updater.write_firmware(ota.next_offset, &chunk).await.map_err(|_| ()).is_err() {
                    ota.abort("flash write failed");
                    return;
                }
                ota.total_write_ticks = ota
                    .total_write_ticks
                    .saturating_add(Instant::now().as_ticks().saturating_sub(write_start_tick));
                ota.chunks_written = ota.chunks_written.saturating_add(1);
                ota.next_offset += chunk_len;
                if ota.next_offset >= ota.next_progress_log_at {
                    log::info!(
                        "ota: progress downloaded={}B req={} resp={}",
                        ota.next_offset,
                        ota.requests_sent,
                        ota.responses_received
                    );
                    ota.next_progress_log_at += OTA_PROGRESS_LOG_STEP;
                }
            }

            if chunk_len < READ_CHUNK_SIZE {
                let elapsed_ticks = Instant::now().as_ticks().saturating_sub(ota.start_tick);
                let elapsed_ms = crate::ota::OtaSession::ticks_to_ms(elapsed_ticks);
                let effective_bitrate_bps = if elapsed_ticks > 0 {
                    (ota.next_offset as u64)
                        .saturating_mul(8)
                        .saturating_mul(TICK_HZ)
                        / elapsed_ticks
                } else {
                    0
                };
                let wait_ms = crate::ota::OtaSession::ticks_to_ms(ota.total_wait_ticks);
                let write_ms = crate::ota::OtaSession::ticks_to_ms(ota.total_write_ticks);
                let other_ms = elapsed_ms.saturating_sub(wait_ms.saturating_add(write_ms));
                let wait_pct = if elapsed_ticks > 0 {
                    ota.total_wait_ticks.saturating_mul(100) / elapsed_ticks
                } else { 0 };
                let write_pct = if elapsed_ticks > 0 {
                    ota.total_write_ticks.saturating_mul(100) / elapsed_ticks
                } else { 0 };
                let avg_chunk_bytes = if ota.chunks_written > 0 {
                    ota.next_offset / ota.chunks_written as usize
                } else { 0 };
                log::info!(
                    "ota: transfer complete bytes={} time_ms={} bitrate={} bps ({} kbps) req={} resp={} chunks={} avg_chunk={}B",
                    ota.next_offset, elapsed_ms, effective_bitrate_bps,
                    effective_bitrate_bps / 1000,
                    ota.requests_sent, ota.responses_received,
                    ota.chunks_written, avg_chunk_bytes
                );
                log::info!(
                    "ota: timing breakdown wait_ms={} ({}%) write_ms={} ({}%) other_ms={}",
                    wait_ms, wait_pct, write_ms, write_pct, other_ms
                );
                match updater.mark_updated().await {
                    Ok(()) => {
                        log::info!("ota: marked updated, rebooting");
                        ota.clear();
                        PENDING_RESET.store(RESET_SOFT, Ordering::Release);
                    }
                    Err(_) => {
                        log::warn!("ota: mark_updated failed");
                        ota.abort("mark_updated failed");
                    }
                }
            }
        } else if now_ticks >= ota.response_deadline_ticks {
            ota.accumulate_wait_ticks(now_ticks);
            ota.waiting_response = false;
            ota.response_timeouts = ota.response_timeouts.saturating_add(1);
            if ota.response_timeouts > OTA_TIMEOUT_RETRY_LIMIT {
                ota.abort("file.read response timeout");
            } else {
                log::warn!(
                    "ota: file.read response timeout at offset={} retry={}/{}",
                    ota.next_offset, ota.response_timeouts, OTA_TIMEOUT_RETRY_LIMIT
                );
            }
            return;
        } else {
            return;
        }
    }

    if !ota.active {
        return;
    }

    // Send the next file.Read request.
    let request = ReadRequest {
        offset: ota.next_offset as u64,
        path: FilePath { path: ota.path.clone() },
    };
    match node.send_request(read_service, &request, ota.server_node) {
        Ok(_) => {
            ota.requests_sent = ota.requests_sent.saturating_add(1);
            ota.waiting_response = true;
            ota.wait_started_tick = Instant::now().as_ticks();
            ota.response_deadline_ticks =
                Instant::now().as_ticks() + OTA_RESPONSE_TIMEOUT.as_ticks();
        }
        Err(nb::Error::WouldBlock) => {
            // TX queue full — retry next iteration.
        }
        Err(nb::Error::Other(_)) => {
            ota.abort("failed to send file.read request");
        }
    }
}

// ---- Main node function ---------------------------------------------------

/// Run the Cyphal node for the lifetime of the application.
///
/// This function **never returns**. Wrap it in your own
/// `#[embassy_executor::task]`.
///
/// # Arguments
/// - `spi_bus` — shared blocking SPI bus mutex (`&'static`)
/// - `cs` — chip-select GPIO for the MCP25xx
/// - `reset` — active-low reset GPIO for the MCP25xx
/// - `int` — active-low interrupt GPIO from the MCP25xx
/// - `flash` — shared flash mutex for OTA firmware writes
/// - `nvs_range` — flash address range erased on `COMMAND_FACTORY_RESET`
/// - `unique_id` — 16-byte device unique ID (from the RP2040 flash UID)
/// - `node_info` — pre-built [`GetInfoResponse`] (see [`crate::NodeInfoConfig`])
/// - `handler` — [`CyphalHandler`] containing the command handler and optional
///   extension; the extension receives any transfers not consumed by the library
pub async fn run_cyphal_node<T, E>(
    spi_bus: &'static SpiBusMutex<T>,
    cs: Output<'static>,
    mut reset: Output<'static>,
    mut int: Input<'static>,
    flash: &'static FlashMutex,
    nvs_range: Range<u32>,
    unique_id: [u8; 16],
    node_info: GetInfoResponse,
    mut handler: CyphalHandler<E>,
) -> !
where
    T: embassy_rp::spi::Instance,
    E: NodeExtension,
{
    let spi = SpiDevice::new(spi_bus, cs);
    let mut mcp25xx = MCP25xx { spi };
    let mut init_attempt: u32 = 0;

    // ---- Phase 0: MCP25xx hardware initialisation -------------------------

    loop {
        init_attempt = init_attempt.wrapping_add(1);
        log::info!("can: init attempt {}", init_attempt);
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

    // ---- Phase 1: PnP dynamic node-ID allocation --------------------------

    let clock = TimerClock;
    let transmitter = CanTransmitter::new(Mtu::Can8);
    let receiver = CanReceiver::new_anonymous();
    let driver = Mcp25xxDriver::new(mcp25xx);
    let queue_driver = SingleQueueDriver::new(ArrayQueue::new(), driver);
    let mut core: Core<T> =
        CoreNode::new_anonymous(clock, transmitter, receiver, queue_driver);

    if core
        .subscribe_message(pnp_v1::SUBJECT, 9, milliseconds(1_000))
        .is_err()
    {
        log::warn!("can: pnp: subscribe_message failed");
        loop { Timer::after_secs(1).await; }
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
        loop { Timer::after_secs(1).await; }
    }

    let pnp_request = PnpMsg {
        unique_id_hash: pnp_unique_id_hash(&unique_id),
        allocated_node_id: heapless::Vec::new(),
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
                Err(nb::Error::WouldBlock) => {}
                Err(nb::Error::Other(err)) => {
                    log::warn!("can: pnp allocation request failed: {:?}", err);
                }
            }
            next_pnp_request_at =
                now + pnp_request_retry_duration(&unique_id, pnp_requests_sent);
        }

        Timer::after(LOOP_SLEEP_IDLE).await;
    };

    log::info!(
        "can: allocated node ID {} after {} request(s)",
        node_id.to_u8(),
        pnp_requests_sent
    );
    ASSIGNED_NODE_ID.store(node_id.to_u8(), Ordering::Release);

    // ---- Phase 2: Promote to named BasicNode ------------------------------

    NodeTrait::set_node_id(&mut core, node_id);

    let mut node: Node<T> = match BasicNode::new(core, node_info) {
        Ok(n) => n,
        Err(_) => {
            log::warn!("can: BasicNode init failed");
            loop { Timer::after_secs(1).await; }
        }
    };

    node.set_health(Health { value: Health::NOMINAL });

    if node
        .subscribe_request(
            canadensis_data_types::uavcan::node::execute_command_1_3::SERVICE,
            EXECUTE_COMMAND_PAYLOAD_MAX,
            milliseconds(2_000),
        )
        .is_err()
    {
        log::warn!("can: subscribe_request execute_command failed");
        loop { Timer::after_secs(1).await; }
    }

    // ---- Extension subscriptions ------------------------------------------
    if !handler.register_subscriptions(&mut node) {
        log::warn!("can: extension register_subscriptions failed");
    }

    // ---- Phase 3: OTA setup -----------------------------------------------

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
        Err(_) => {
            log::warn!("boot state read failed");
            false
        }
    };
    if mark_boot {
        if let Err(_) = updater.mark_booted().await {
            log::warn!("boot state mark_booted failed");
        }
    }

    let read_service = match node.start_sending_requests::<ReadRequest>(
        canadensis_data_types::uavcan::file::read_1_1::SERVICE,
        milliseconds(2_000),
        READ_RESPONSE_PAYLOAD_MAX,
        Priority::Nominal,
    ) {
        Ok(token) => token,
        Err(err) => {
            log::warn!("can: start_sending_requests file.read failed: {:?}", err);
            loop { Timer::after_secs(1).await; }
        }
    };

    log::info!("can: initialized");

    // ---- Phase 4: Main loop -----------------------------------------------

    let mut next_per_second = Instant::now() + Duration::from_secs(1);

    loop {
        // ---- Handle pending reset ------------------------------------------
        let reset_mode = PENDING_RESET.swap(RESET_NONE, Ordering::AcqRel);
        if reset_mode != RESET_NONE {
            // Flush TX twice so the ExecuteCommand response frames reach the bus
            // before any long blocking operation.
            for _ in 0..2 {
                let _ = node.node_mut().flush();
                Timer::after_millis(100).await;
            }

            if reset_mode == RESET_FACTORY {
                let result = {
                    let mut f = flash.lock().await;
                    f.blocking_erase(nvs_range.start, nvs_range.end)
                };
                match result {
                    Ok(()) => log::info!("factory reset erased NVS region"),
                    Err(_) => log::error!("factory reset erase failed"),
                }
            }

            let _ = node.node_mut().flush();
            Timer::after_millis(100).await;
            cortex_m::peripheral::SCB::sys_reset();
        }

        // ---- Wait for CAN activity ----------------------------------------
        // If INT is already low, skip the await and drain immediately.
        if int.is_high() {
            let can_wait_timeout = if handler.is_ota_active() {
                CAN_WAIT_TIMEOUT_OTA
            } else {
                CAN_WAIT_TIMEOUT_IDLE
            };
            let _ = with_timeout(can_wait_timeout, int.wait_for_low()).await;
        }

        // ---- Per-second maintenance (heartbeat, error reporting) ----------
        let now = Instant::now();
        while now >= next_per_second {
            let _ = node.run_per_second_tasks();
            if let Some(status) = node.node_mut().driver_mut().driver_mut().read_status() {
                if status.txbo {
                    log::warn!("can: BUS-OFF! Check bitrate (16MHz crystal?) and termination.");
                } else if status.txep || status.rxep {
                    log::warn!(
                        "can: error-passive txep={} rxep={} tec={} rec={} tx0if={}",
                        status.txep, status.rxep, status.tec, status.rec, status.tx0if,
                    );
                } else if status.txwar || status.rxwar {
                    log::warn!(
                        "can: error-warning txwar={} rxwar={} tec={} rec={} tx0if={}",
                        status.txwar, status.rxwar, status.tec, status.rec, status.tx0if,
                    );
                }
                if status.txbo || status.txep || status.txwar {
                    log::warn!(
                        "can: txb0 req={} err={} mloa={} abtf={} txif={}",
                        status.txb0.txreq, status.txb0.txerr,
                        status.txb0.mloa, status.txb0.abtf, status.txb0.txif,
                    );
                    if status.txb0.txreq {
                        let recovered = node
                            .node_mut()
                            .driver_mut()
                            .driver_mut()
                            .abort_pending_transmissions();
                        log::warn!("can: abort pending tx recovered={}", recovered);
                    }
                }
            }
            next_per_second += Duration::from_secs(1);
        }

        // ---- Drain incoming frames ----------------------------------------
        // During OTA, response frames can arrive in bursts. Drain multiple
        // rounds, but cap the rounds so OTA request progress never starves.
        let max_rounds = if handler.is_ota_active() {
            1 + RX_DRAIN_EXTRA_ROUNDS_OTA
        } else {
            1
        };
        for _ in 0..max_rounds {
            let mut drained_any = false;
            for _ in 0..RX_DRAIN_BUDGET {
                if node.receive(&mut handler).is_err() {
                    break;
                }
                drained_any = true;
            }
            if !drained_any {
                break;
            }
        }

        // ---- Drive OTA state machine --------------------------------------
        drive_ota_step(&mut handler, &mut node, &read_service, &mut updater).await;

        // Flush immediately after OTA requests to minimise request/response latency.
        let _ = node.node_mut().flush();

        // ---- Update node mode (SOFTWARE_UPDATE during OTA) ----------------
        if handler.is_ota_active() {
            node.set_mode(Mode { value: Mode::SOFTWARE_UPDATE });
        } else {
            node.set_mode(Mode { value: Mode::OPERATIONAL });
        }

        let _ = node.node_mut().flush();

        let ota_active = handler.is_ota_active();
        // While OTA is active and INT remains asserted, continue immediately
        // to keep draining burst traffic without an added sleep gap.
        if ota_active && int.is_low() {
            continue;
        }
        let loop_sleep = if ota_active {
            LOOP_SLEEP_OTA
        } else {
            LOOP_SLEEP_IDLE
        };
        Timer::after(loop_sleep).await;
    }
}
