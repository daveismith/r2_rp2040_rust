use embassy_time::Timer;
/// ISO-TP (ISO 15765-2) transport-layer wrapper for any `embedded_can::blocking::Can`.
/// Supports segmentation, reassembly, and basic flow control (CTS only).
use embedded_can::{Frame, Id};
use heapless::Vec;
use core::cmp::min;

const MAX_SINGLE_FRAME_DATA: usize = 7;
const MAX_FIRST_FRAME_DATA: usize = 6;
const MAX_CONSECUTIVE_FRAME_DATA: usize = 7;
pub const MAX_PAYLOAD_SIZE: usize = 4095;

#[derive(Debug)]
pub enum IsoTpError {
    FrameTooLarge,
    InvalidFrame,
    UnexpectedFrame,
    CanTransmitError,
    UnexpectedFlowStatus,
}

#[derive(Debug)]
pub enum IsoTpMessage {
    /// A complete reassembled payload
    Complete(Vec<u8, MAX_PAYLOAD_SIZE>),
    /// More segments are still coming
    InProgress,
}

#[derive(Debug)]
struct RxSession {
    buffer: Vec<u8, MAX_PAYLOAD_SIZE>,
    expected: u8,
    total: usize,
}

#[derive(Debug)]
struct TxSession {
    buffer: Vec<u8, MAX_PAYLOAD_SIZE>,
    offset: usize,
    sn: u8,
}

/// ISO-TP node: owns a CAN interface, single RX/TX session.
pub struct IsoTpNode<'a, F>
where 
    F: embedded_can::Frame
{
    tx_queue: embassy_sync::channel::DynamicSender<'a, F>,
    //can: &'a Mutex<M, C>,
    tx_id: Id,
    rx_id: Id,
    rx_session: Option<RxSession>,
    tx_session: Option<TxSession>,
}

impl<'a, F> IsoTpNode<'a, F>
where
    F: embedded_can::Frame,
{
    /// Create a new ISO-TP node.
    /// `tx_id` is the CAN ID you send frames to.
    /// `rx_id` is the CAN ID you listen on for incoming frames.
    //pub fn new(can: &'a Mutex<M, C>, tx_id: Id, rx_id: Id) -> Self {
    //    Self { can, tx_id, rx_id, rx_session: None, tx_session: None }
    //}
    pub fn new(tx_queue: embassy_sync::channel::DynamicSender<'a, F>, tx_id: Id, rx_id: Id) -> Self {
        Self { tx_queue, tx_id, rx_id, rx_session: None, tx_session: None }
    }

    /// Change the transmit CAN ID.
    pub fn set_tx_id(&mut self, id: Id) {
        self.tx_id = id;
    }

    /// Change the receive CAN ID.
    pub fn set_rx_id(&mut self, id: Id) {
        self.rx_id = id;
    }

    /// Send a payload; single‑frame or segmented.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), IsoTpError> {
        // clear any previous TX session
        self.tx_session = None;

        let len = data.len();
        if len > MAX_PAYLOAD_SIZE {
            return Err(IsoTpError::FrameTooLarge);
        }

        // Single-Frame
        if len <= MAX_SINGLE_FRAME_DATA {
            let mut buf = [0u8; 8];
            buf[0] = (len & 0x0F) as u8;
            buf[1..1 + len].copy_from_slice(data);

            let frame = F::new(self.tx_id, &buf).unwrap();
            return match self.tx_queue.try_send(frame) {
                Ok(_) => Ok(()),
                Err(_) => Err(IsoTpError::CanTransmitError)
            }
            /*return match self.can.try_lock() {
                Ok(mut guard) => {
                    let frame = C::Frame::new(self.tx_id, &buf).unwrap();
                    guard.transmit(&frame).map_err(|_| IsoTpError::CanTransmitError)
                },
                Err(_) => Err(IsoTpError::CanTransmitError)
            }*/
        }

        // First Frame
        let mut session = TxSession { buffer: Vec::new(), offset: MAX_FIRST_FRAME_DATA, sn: 1 };
        session.buffer.extend_from_slice(data).map_err(|_| IsoTpError::FrameTooLarge)?;

        let mut ff = [0u8; 8];
        ff[0] = 0x10 | (((len >> 8) & 0x0F) as u8);
        ff[1] = (len & 0xFF) as u8;
        ff[2..8].copy_from_slice(&data[..MAX_FIRST_FRAME_DATA]);

        let frame = F::new(self.tx_id, &ff).unwrap();
        return match self.tx_queue.try_send(frame) {
            Ok(_) => {
                self.tx_session = Some(session);
                Ok(())
            },
            Err(_) => Err(IsoTpError::CanTransmitError)
        }

        /*return match self.can.try_lock() {
            Ok(mut guard) => {
                let frame = C::Frame::new(self.tx_id, &ff).unwrap();
                let result = guard.transmit(&frame).map_err(|_| IsoTpError::CanTransmitError);
                if result.is_ok() {
                    self.tx_session = Some(session);
                }
                result
            },
            Err(_) => Err(IsoTpError::CanTransmitError)
        };*/

    }

    /// Process an incoming CAN frame. Returns an ISO-TP message state.
    /// Ignores frames not matching `rx_id`/`tx_id`.
    pub async fn receive(&mut self, frame: &impl Frame) -> Result<Option<IsoTpMessage>, IsoTpError> {
        let id = frame.id();

        if id != self.rx_id && id != self.tx_id {
            return Err(IsoTpError::InvalidFrame)
        }
        let data = frame.data();

        match data[0] >> 4 {
            // Single Frame
            0x0 => {
                let len = (data[0] & 0x0F) as usize;
                let buf = Vec::<u8, MAX_PAYLOAD_SIZE>::from_slice(&data[1..1 + len])
                    .map_err(|_| IsoTpError::FrameTooLarge)
                    .map(|val| Some(IsoTpMessage::Complete(val)));
                self.rx_session = None;
                buf
            }

            // First Frame
            0x1 => {
                let total = (((data[0] & 0x0F) as usize) << 8) | (data[1] as usize);
                let mut sess = RxSession { buffer: Vec::<u8, MAX_PAYLOAD_SIZE>::new(), expected: 1, total };
                
                sess.buffer.extend_from_slice(&data[2..8]).map_err(|_| IsoTpError::FrameTooLarge)?;
                self.rx_session = Some(sess);

                // send CTS
                let mut fc = [0u8; 8];
                fc[0] = 0x30;
                
                let fcf = F::new(self.tx_id, &fc).unwrap();
                match self.tx_queue.try_send(fcf) {
                    Ok(_) => Ok(Some(IsoTpMessage::InProgress)),
                    Err(_) => Err(IsoTpError::CanTransmitError)
                }
                /*match self.can.try_lock() {
                    Ok(mut guard) => {
                        let fcf = C::Frame::new(self.tx_id, &fc).unwrap();
                        match guard.transmit(&fcf) {
                            Ok(_) => Ok(Some(IsoTpMessage::InProgress)),
                            Err(_err) => Err(IsoTpError::CanTransmitError)
                        }
                    },
                    Err(_) => Err(IsoTpError::CanTransmitError)
                }*/
            }

            // Consecutive Frame
            0x2 => {
                let sn = data[0] & 0x0F;
                let sess = self.rx_session.as_mut().ok_or(IsoTpError::UnexpectedFrame)?;
                if sn != sess.expected {
                    return Err(IsoTpError::UnexpectedFrame);
                }
                sess.expected = (sess.expected + 1) & 0x0F;
                sess.buffer.extend_from_slice(&data[1..]).map_err(|_| IsoTpError::FrameTooLarge)?;
                if sess.buffer.len() >= sess.total {
                    let complete = sess.buffer.clone();
                    self.rx_session = None;
                    return Ok(Some(IsoTpMessage::Complete(complete)));
                }

                Ok(Some(IsoTpMessage::InProgress))
            }

            // Flow Control Frame
            0x3 => {
                let fs = data[0] & 0x0F;
                let txs = self.tx_session.as_mut().ok_or(IsoTpError::UnexpectedFrame)?;
                match fs {
                    0x0 => {
                        // CTS: send remaining CFs
                        while txs.offset < txs.buffer.len() {
                            let chunk = min(MAX_CONSECUTIVE_FRAME_DATA, txs.buffer.len() - txs.offset);
                            let mut cf = [0u8; 8];
                            cf[0] = 0x20 | (txs.sn & 0x0F);
                            cf[1..1 + chunk].copy_from_slice(&txs.buffer[txs.offset..txs.offset + chunk]);
                            
                            let f = F::new(self.tx_id, &cf);
                            self.tx_queue.try_send(f.unwrap()).map_err(|_| IsoTpError::CanTransmitError)?;

                            /*{
                                let mut guard = self.can.lock().await;
                                let f = <C::Frame as Frame>::new(self.tx_id, &cf);
                                guard.transmit(&f.unwrap()).map_err(|_| IsoTpError::CanTransmitError)?;
                            }
                            */
                            txs.offset += chunk;
                            txs.sn = (txs.sn + 1) & 0x0F;
                            Timer::after_millis(10).await;
                        }
                        self.tx_session = None;
                        Ok(None)
                    }
                    0x1 => Ok(None), // wait
                    _ => Err(IsoTpError::UnexpectedFlowStatus),
                }
            }

            _ => Err(IsoTpError::InvalidFrame),
        }

    }
}
