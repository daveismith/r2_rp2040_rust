//! Cyphal node task for the `shoulder-sensor` application.
//!
//! # Published subjects
//!
//! | Type                                   | Default subject ID | Rate   | Unit    |
//! |----------------------------------------|--------------------|--------|---------|
//! | `uavcan.si.unit.angle.Scalar` v1.0     | 6144               | 100 Hz | radians |
//! | `uavcan.si.unit.temperature.Scalar`v1.0| 6145               | 1 Hz   | kelvin  |
//!
//! The receiver identifies the joint by the **source node ID** carried in
//! every Cyphal transfer header; the subject ID is the same for all nodes
//! running this firmware.
//!
//! # Vendor commands (ExecuteCommand)
//!
//! | Command code | Name           | Effect                                          |
//! |--------------|----------------|-------------------------------------------------|
//! | `0x0001`     | `COMMAND_ZERO` | Set the current angle as the new zero reference |
//!
//! # Rate management
//!
//! [`ShoulderExtension::on_tick`] is called on every iteration of the
//! [`cyphal_node::run_cyphal_node`] main loop (up to
//! [`ShoulderExtension::preferred_loop_period`] Hz).  The extension tracks
//! two independent `Instant`-based deadlines and publishes only when the
//! respective deadline has elapsed.

use core::sync::atomic::Ordering;

use canadensis::core::time::milliseconds;
use canadensis::core::transfer::{MessageTransfer, ServiceTransfer};
use canadensis::core::{Priority, SubjectId};
use canadensis::{ResponseToken, TransferHandler};
use canadensis_can::CanTransport;
use canadensis_data_types::uavcan::node::execute_command_1_3::{
    ExecuteCommandRequest, ExecuteCommandResponse,
};
use canadensis_data_types::uavcan::si::unit::angle::scalar_1_0::Scalar as AngleScalar;
use canadensis_data_types::uavcan::si::unit::temperature::scalar_1_0::Scalar as TempScalar;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::peripherals;
use embassy_time::{Duration, Instant};

use cyphal_node::{
    node_task::{FlashMutex, SpiBusMutex, run_cyphal_node},
    CyphalHandler, DefaultCommandHandler, NodeExtension, NodeInfoConfig,
};

use crate::built_info;
use crate::{ANGLE_SUBJECT_ID, NVS_RANGE, PENDING_ZERO_PERSIST, TEMP_SUBJECT_ID, TLV_ANGLE, TLV_TEMP, ZERO_OFFSET};

// ---- Vendor-specific command code -----------------------------------------

/// Vendor command: set the current angle as the zero reference.
///
/// The command code must be in the range 0x0000–0x7FFF (vendor-specific).
const COMMAND_ZERO: u16 = 0x0001;

// ---- Angle maths ----------------------------------------------------------

/// Wrap an angle in radians to the range (−π, +π].
fn wrap_angle(rad: f32) -> f32 {
    use core::f32::consts::PI;
    let mut a = rad;
    while a > PI {
        a -= 2.0 * PI;
    }
    while a <= -PI {
        a += 2.0 * PI;
    }
    a
}

// ---- Application-specific transfer handler --------------------------------

/// Application extension for the shoulder-sensor Cyphal node.
///
/// Publishes angle (100 Hz) and temperature (1 Hz) data, and handles the
/// vendor `COMMAND_ZERO` executive command.
pub struct ShoulderExtension {
    /// Deadline for the next angle publication.
    next_angle_at: Instant,
    /// Deadline for the next temperature publication.
    next_temp_at: Instant,
}

impl ShoulderExtension {
    /// Create a new extension with both publication deadlines set to now so
    /// the first message is sent on the first tick.
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            next_angle_at: now,
            next_temp_at: now,
        }
    }
}

impl TransferHandler<CanTransport> for ShoulderExtension {
    fn handle_message<N>(
        &mut self,
        _node: &mut N,
        _transfer: &MessageTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }

    fn handle_request<N>(
        &mut self,
        _node: &mut N,
        _token: ResponseToken<CanTransport>,
        _transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }

    fn handle_response<N>(
        &mut self,
        _node: &mut N,
        _transfer: &ServiceTransfer<alloc::vec::Vec<u8>, CanTransport>,
    ) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        false
    }
}

// ---- NodeExtension impl ---------------------------------------------------

impl NodeExtension for ShoulderExtension {
    fn register_subscriptions<N>(&self, _node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        // No incoming subscriptions.
        true
    }

    fn register_publishers<N>(&self, node: &mut N) -> bool
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        let angle_subject =
            SubjectId::from_truncating(ANGLE_SUBJECT_ID.load(Ordering::Relaxed));
        let temp_subject =
            SubjectId::from_truncating(TEMP_SUBJECT_ID.load(Ordering::Relaxed));

        let ok1 = node
            .start_publishing(angle_subject, milliseconds(100), Priority::Nominal.into())
            .is_ok();
        let ok2 = node
            .start_publishing(temp_subject, milliseconds(1_000), Priority::Nominal.into())
            .is_ok();

        if !ok1 {
            log::warn!("shoulder: failed to register angle publisher");
        }
        if !ok2 {
            log::warn!("shoulder: failed to register temperature publisher");
        }
        ok1 && ok2
    }

    /// Called on every node loop iteration.
    ///
    /// Checks both rate deadlines; publishes whichever messages are due.
    /// Deadlines are advanced like `embassy_time::Ticker` — relative to the
    /// previous deadline (`+= period`) rather than to the current time
    /// (`= now + period`).  This prevents drift accumulation across many
    /// iterations.  A catch-up guard resets the deadline to `now + period`
    /// when the loop has fallen more than one period behind, so a burst of
    /// back-to-back publications never occurs.
    /// Does not block or await.
    fn on_tick<N>(&mut self, node: &mut N)
    where
        N: canadensis::Node<Transport = CanTransport>,
    {
        let now = Instant::now();

        // ---- Angle publication (100 Hz) -----------------------------------
        if now >= self.next_angle_at {
            let measured = f32::from_bits(TLV_ANGLE.load(Ordering::Relaxed));
            let offset = f32::from_bits(ZERO_OFFSET.load(Ordering::Relaxed));
            let value = wrap_angle(measured - offset);

            let subject =
                SubjectId::from_truncating(ANGLE_SUBJECT_ID.load(Ordering::Relaxed));
            let msg = AngleScalar { radian: value };
            let _ = node.publish(subject, &msg);

            // Advance deadline like a Ticker: from the last scheduled time,
            // not from `now`, to avoid drift.
            self.next_angle_at += Duration::from_hz(100);
            // Catch-up guard: if we have fallen more than one period behind
            // (e.g. after a long blocking operation), reset to avoid a burst.
            if self.next_angle_at < now {
                self.next_angle_at = now + Duration::from_hz(100);
            }
        }

        // ---- Temperature publication (1 Hz) --------------------------------
        if now >= self.next_temp_at {
            let raw_temp = TLV_TEMP.load(Ordering::Relaxed);
            let kelvin = (raw_temp as f32 / 100.0) + 273.15;

            let subject =
                SubjectId::from_truncating(TEMP_SUBJECT_ID.load(Ordering::Relaxed));
            let msg = TempScalar { kelvin };
            let _ = node.publish(subject, &msg);

            // Same Ticker-style advancement for the temperature deadline.
            self.next_temp_at += Duration::from_hz(1);
            if self.next_temp_at < now {
                self.next_temp_at = now + Duration::from_hz(1);
            }
        }
    }

    /// Handle the vendor `COMMAND_ZERO` (0x0001): set the current measured
    /// angle as the zero reference.
    ///
    /// Updates the in-memory `ZERO_OFFSET` atomic immediately; NVS
    /// persistence is deferred to the `settings_persist_task`.
    fn handle_execute_command(
        &mut self,
        request: &ExecuteCommandRequest,
    ) -> Option<(u8, &'static [u8])> {
        if request.command == COMMAND_ZERO {
            let current_bits = TLV_ANGLE.load(Ordering::Relaxed);
            ZERO_OFFSET.store(current_bits, Ordering::Relaxed);
            // Signal the persist task to write the new offset to NVS.
            PENDING_ZERO_PERSIST.store(true, Ordering::Release);
            log::info!(
                "shoulder: COMMAND_ZERO — zero offset set to {:?} rad",
                f32::from_bits(current_bits)
            );
            Some((ExecuteCommandResponse::STATUS_SUCCESS, b"zeroed"))
        } else {
            None
        }
    }

    /// Request a loop period that guarantees at least two iterations per
    /// angle-publish interval.
    ///
    /// The angle is published at 100 Hz (every 10 ms).  Using half that
    /// period (5 ms / 200 Hz) ensures the main loop visits `on_tick` at
    /// roughly twice the publish rate, giving Ticker-style precision: even
    /// if one iteration fires a little late the next deadline is still
    /// caught within the same 10 ms window.
    fn preferred_loop_period(&self) -> Duration {
        Duration::from_hz(200)
    }
}

// ---- Node-ID helper -------------------------------------------------------

/// Returns the dynamically allocated Cyphal node ID, or `None` if PnP
/// allocation has not yet completed.
pub fn assigned_node_id() -> Option<u8> {
    cyphal_node::assigned_node_id()
}

// ---- Embassy task ---------------------------------------------------------

/// Main Cyphal task for the shoulder-sensor.
///
/// Builds node info from the app's Cargo metadata, wires the
/// [`ShoulderExtension`] handler, then delegates to
/// [`run_cyphal_node`] for the full node lifecycle.
#[embassy_executor::task]
pub async fn can_handler(
    spi_bus: &'static SpiBusMutex<peripherals::SPI1>,
    cs: Output<'static>,
    reset: Output<'static>,
    int: Input<'static>,
    flash: &'static FlashMutex,
    unique_id: [u8; 16],
) {
    let node_info = NodeInfoConfig::new(unique_id)
        .with_vcs_revision_id(
            built_info::GIT_COMMIT_HASH_SHORT
                .and_then(|h| u64::from_str_radix(h, 16).ok())
                .unwrap_or(0),
        )
        .build_with_env(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
        );

    let command_handler = DefaultCommandHandler::new(None, NVS_RANGE);
    let handler = CyphalHandler::new(command_handler, ShoulderExtension::new());

    run_cyphal_node(
        spi_bus,
        cs,
        reset,
        int,
        flash,
        NVS_RANGE,
        unique_id,
        node_info,
        handler,
    )
    .await;
}

// ---- Settings persistence task --------------------------------------------

/// Loads NVS settings at startup, then watches `PENDING_ZERO_PERSIST` and
/// flushes changed zero offsets to NVS.
///
/// Loading happens in this task (not in `main`) because async flash
/// operations require the Embassy executor to be running.  The Cyphal PnP
/// allocation phase takes several seconds, so settings are available well
/// before any publishing begins.
///
/// `handle_execute_command` cannot perform async flash writes; it updates
/// the in-memory atomic and sets the flag so this task persists the value.
#[embassy_executor::task]
pub async fn settings_persist_task(flash: &'static FlashMutex) {
    use core::ops::DerefMut;
    use embassy_time::Timer;

    // ---- Startup: load settings from NVS ----------------------------------
    {
        let mut f = flash.lock().await;
        let angle_id = crate::settings::fetch_u32(
            f.deref_mut(),
            NVS_RANGE,
            crate::settings::ShoulderSettings::AngleSubjectId,
            crate::settings::DEFAULT_ANGLE_SUBJECT_ID as u32,
        )
        .await;
        let temp_id = crate::settings::fetch_u32(
            f.deref_mut(),
            NVS_RANGE,
            crate::settings::ShoulderSettings::TempSubjectId,
            crate::settings::DEFAULT_TEMP_SUBJECT_ID as u32,
        )
        .await;
        let zero_bits = crate::settings::fetch_u32(
            f.deref_mut(),
            NVS_RANGE,
            crate::settings::ShoulderSettings::ZeroOffset,
            crate::settings::DEFAULT_ZERO_OFFSET_BITS,
        )
        .await;

        ANGLE_SUBJECT_ID.store(angle_id as u16, Ordering::Relaxed);
        TEMP_SUBJECT_ID.store(temp_id as u16, Ordering::Relaxed);
        ZERO_OFFSET.store(zero_bits, Ordering::Relaxed);
        log::info!(
            "shoulder: NVS loaded — angle_subject={} temp_subject={} zero_offset={:?} rad",
            angle_id,
            temp_id,
            f32::from_bits(zero_bits)
        );
    }

    // ---- Persistence loop -------------------------------------------------
    loop {
        if PENDING_ZERO_PERSIST.swap(false, Ordering::AcqRel) {
            let zero_bits = ZERO_OFFSET.load(Ordering::Relaxed);
            let mut f = flash.lock().await;
            let result = crate::settings::store_u32(
                f.deref_mut(),
                NVS_RANGE,
                crate::settings::ShoulderSettings::ZeroOffset,
                zero_bits,
            )
            .await;
            match result {
                Ok(()) => log::info!("shoulder: zero offset persisted to NVS"),
                Err(()) => log::warn!("shoulder: NVS write failed for zero offset"),
            }
        }
        Timer::after_millis(100).await;
    }
}
