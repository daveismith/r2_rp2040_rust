//! MCP25625 / MCP2515 CAN transceiver driver for the Cyphal stack.
//!
//! Wraps [`mcp25xx::MCP25xx`] to implement the `canadensis_can` transmit and
//! receive driver traits, enabling the Cyphal stack to use the hardware CAN
//! controller over SPI.

use canadensis::core::OutOfMemoryError;
use canadensis::core::subscription::Subscription;
use canadensis::core::time::Microseconds32;
use canadensis::nb;
use canadensis_can::driver::{ReceiveDriver, TransmitDriver};
use canadensis_can::{CanId, CanNodeId, Frame};
use embedded_can::{ExtendedId, Frame as _};
use mcp25xx::registers::{CANCTRL, CANINTF, EFLG, REC, TEC, TXB0CTRL};
use mcp25xx::{MCP25xx, TxBuffer};
use embedded_can::nb::Can as EmbeddedNbCan;
use portable_atomic::{AtomicU32, Ordering};

use crate::clock::TimerClock;

/// Total number of CAN frame transmit attempts.
pub static CAN_TX_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
/// Number of frames successfully placed in the TX buffer.
pub static CAN_TX_OK: AtomicU32 = AtomicU32::new(0);
/// Number of transmit attempts that would have blocked (TX buffer busy).
pub static CAN_TX_WOULD_BLOCK: AtomicU32 = AtomicU32::new(0);
/// Number of frames successfully received.
pub static CAN_RX_OK: AtomicU32 = AtomicU32::new(0);
/// Number of receive attempts that returned WouldBlock (no frame available).
pub static CAN_RX_WOULD_BLOCK: AtomicU32 = AtomicU32::new(0);

/// Snapshot of the MCP25xx error and interrupt flag registers.
#[derive(Clone, Copy, Debug)]
pub struct TxBufferStatus {
    /// Transmit-request pending.
    pub txreq: bool,
    /// TX error detected.
    pub txerr: bool,
    /// Message lost arbitration.
    pub mloa: bool,
    /// Transmission aborted.
    pub abtf: bool,
    /// TX0 interrupt flag.
    pub txif: bool,
}

/// Combined CAN controller status snapshot (EFLG + CANINTF + TXB0CTRL).
#[derive(Clone, Copy, Debug)]
pub struct ControllerStatus {
    /// Bus-off state (error count saturated).
    pub txbo: bool,
    /// TX error-passive.
    pub txep: bool,
    /// RX error-passive.
    pub rxep: bool,
    /// TX error warning threshold reached.
    pub txwar: bool,
    /// RX error warning threshold reached.
    pub rxwar: bool,
    /// TX error counter.
    pub tec: u8,
    /// RX error counter.
    pub rec: u8,
    /// Status of TX buffer 0.
    pub txb0: TxBufferStatus,
    /// TX0 interrupt flag (copy from CANINTF).
    pub tx0if: bool,
}

/// Driver wrapper that adapts [`MCP25xx`] to the `canadensis_can` driver traits.
///
/// Implements both [`TransmitDriver`] and [`ReceiveDriver`] for use with the
/// Cyphal stack's [`canadensis_can::queue::SingleQueueDriver`].
pub struct Mcp25xxDriver<SPI: embedded_hal::spi::SpiDevice> {
    controller: MCP25xx<SPI>,
}

impl<SPI: embedded_hal::spi::SpiDevice> Mcp25xxDriver<SPI> {
    /// Create a new driver wrapping the given MCP25xx instance.
    pub fn new(controller: MCP25xx<SPI>) -> Self {
        Self { controller }
    }

    /// Read a combined status snapshot from the controller registers.
    ///
    /// Returns `None` if any SPI register read fails.
    pub fn read_status(&mut self) -> Option<ControllerStatus> {
        let eflg = self.controller.read_register::<EFLG>().ok()?;
        let tec = self.controller.read_register::<TEC>().ok()?.0;
        let rec = self.controller.read_register::<REC>().ok()?.0;
        let canintf = self.controller.read_register::<CANINTF>().ok()?;
        Some(ControllerStatus {
            txbo: eflg.txbo(),
            txep: eflg.txep(),
            rxep: eflg.rxep(),
            txwar: eflg.txwar(),
            rxwar: eflg.rxwar(),
            tec,
            rec,
            txb0: self.read_tx_buffer_status(canintf.tx0if())?,
            tx0if: canintf.tx0if(),
        })
    }

    /// Abort any pending transmission in TX buffer 0 and clear error flags.
    ///
    /// Returns `true` if all register operations succeeded.
    pub fn abort_pending_transmissions(&mut self) -> bool {
        if self
            .controller
            .modify_register(CANCTRL::new().with_abat(true), 0b0001_0000)
            .is_err()
        {
            return false;
        }

        let clear_mask = 0b0111_1000;
        if self.controller.modify_register(TXB0CTRL::new(), clear_mask).is_err() {
            return false;
        }
        if self.controller.modify_register(CANINTF::new(), 0b0000_0100).is_err() {
            return false;
        }
        self.controller.modify_register(CANCTRL::new(), 0b0001_0000).is_ok()
    }

    fn read_tx_buffer_status(&mut self, txif: bool) -> Option<TxBufferStatus> {
        let txb = self.controller.read_register::<TXB0CTRL>().ok()?;
        Some(TxBufferStatus {
            txreq: txb.txreq(),
            txerr: txb.txerr(),
            mloa: txb.mloa(),
            abtf: txb.abtf(),
            txif,
        })
    }

    fn txb0_busy(&mut self) -> bool {
        self.controller
            .read_register::<TXB0CTRL>()
            .map(|r| r.txreq())
            .unwrap_or(true)
    }

    fn clear_tx0if(&mut self) {
        // CANINTF: TX0IF is bit 2
        let _ = self.controller.modify_register(CANINTF::new(), 1 << 2);
    }

    fn tx_would_block<T>() -> nb::Result<T, core::convert::Infallible> {
        CAN_TX_WOULD_BLOCK.fetch_add(1, Ordering::Relaxed);
        Err(nb::Error::WouldBlock)
    }

    fn rx_would_block<T>() -> nb::Result<T, core::convert::Infallible> {
        CAN_RX_WOULD_BLOCK.fetch_add(1, Ordering::Relaxed);
        Err(nb::Error::WouldBlock)
    }

    fn convert_incoming(frame: mcp25xx::CanFrame, timestamp: Microseconds32) -> Option<Frame> {
        let embedded_can::Id::Extended(extended_id) = frame.id() else {
            return None;
        };
        let can_id = CanId::try_from(extended_id.as_raw()).ok()?;
        Some(Frame::new(timestamp, can_id, frame.data()))
    }
}

impl<SPI> TransmitDriver<TimerClock> for Mcp25xxDriver<SPI>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    type Error = core::convert::Infallible;

    fn try_reserve(&mut self, _frames: usize) -> Result<(), OutOfMemoryError> {
        Ok(())
    }

    fn transmit(
        &mut self,
        frame: Frame,
        _clock: &mut TimerClock,
    ) -> nb::Result<Option<Frame>, Self::Error> {
        CAN_TX_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        let Some(extended_id) = ExtendedId::new(u32::from(frame.id())) else {
            return Self::tx_would_block();
        };
        let Some(driver_frame) = mcp25xx::CanFrame::new(extended_id, frame.data()) else {
            return Self::tx_would_block();
        };

        if self.txb0_busy() {
            return Self::tx_would_block();
        }

        // Clear stale completion flag before reusing TXB0.
        self.clear_tx0if();

        if self.controller.load_tx_buffer(TxBuffer::TXB0, &driver_frame).is_err() {
            return Self::tx_would_block();
        }
        if self.controller.request_to_send(TxBuffer::TXB0).is_err() {
            return Self::tx_would_block();
        }

        CAN_TX_OK.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    fn flush(&mut self, _clock: &mut TimerClock) -> nb::Result<(), Self::Error> {
        Ok(())
    }
}

impl<SPI> ReceiveDriver<TimerClock> for Mcp25xxDriver<SPI>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    type Error = core::convert::Infallible;

    fn receive(&mut self, clock: &mut TimerClock) -> nb::Result<Frame, Self::Error> {
        use canadensis::core::time::Clock;
        let now = clock.now();
        match EmbeddedNbCan::receive(&mut self.controller) {
            Ok(frame) => {
                if let Some(converted) = Self::convert_incoming(frame, now) {
                    CAN_RX_OK.fetch_add(1, Ordering::Relaxed);
                    Ok(converted)
                } else {
                    Self::rx_would_block()
                }
            }
            Err(nb::Error::WouldBlock) => Self::rx_would_block(),
            Err(nb::Error::Other(_)) => Self::rx_would_block(),
        }
    }

    fn apply_filters<S>(&mut self, _local_node: Option<CanNodeId>, _subscriptions: S)
    where
        S: IntoIterator<Item = Subscription>,
    {
    }

    fn apply_accept_all(&mut self) {}
}
