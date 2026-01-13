//! ISO-TP transport built on `embedded-can-interface`.
//! Exposes blocking and polling helpers with configurable timing and buffering.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(any(feature = "alloc", feature = "std"))]
extern crate alloc;

pub mod config;
pub mod errors;
pub mod pdu;
pub mod rx;
pub mod timer;
pub mod tx;

pub use config::IsoTpConfig;
pub use errors::{IsoTpError, TimeoutKind};
pub use timer::{Clock, StdClock};
pub use tx::Progress;

use core::mem;
use core::time::Duration;
use embedded_can::Frame;
use embedded_can_interface::{Id, RxFrameIo, TxFrameIo};

use pdu::{FlowStatus, Pdu, decode, duration_to_st_min, encode, st_min_to_duration};
use rx::{RxBuffer, RxMachine, RxOutcome};
use tx::{TxSession, TxState};

#[cfg(any(feature = "alloc", feature = "std"))]
use alloc::vec::Vec;

/// Alias for CAN identifier.
pub type CanId = Id;
/// Re-export of the CAN frame trait.
pub use embedded_can::Frame as CanFrame;

/// ISO-TP endpoint backed by split transmit/receive halves and a clock.
pub struct IsoTpNode<'a, Tx, Rx, F, C>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    C: Clock,
{
    tx: Tx,
    rx: Rx,
    cfg: IsoTpConfig,
    clock: C,
    tx_state: TxState<C::Instant>,
    rx_machine: RxMachine<'a>,
}

impl<'a, Tx, Rx, F, C> IsoTpNode<'a, Tx, Rx, F, C>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    /// Construct using a provided clock and optional caller buffer.
    pub fn with_clock(
        tx: Tx,
        rx: Rx,
        cfg: IsoTpConfig,
        clock: C,
        buffer: Option<&'a mut [u8]>,
    ) -> Result<Self, IsoTpError<()>> {
        cfg.validate().map_err(|_| IsoTpError::InvalidConfig)?;
        let rx_buffer = build_buffer(cfg.rx_buffer_len, buffer)?;
        Ok(Self {
            tx,
            rx,
            cfg,
            clock,
            tx_state: TxState::Idle,
            rx_machine: RxMachine::new(rx_buffer),
        })
    }

    /// Advance transmission once; caller supplies current time.
    pub fn poll_send(
        &mut self,
        payload: &[u8],
        now: C::Instant,
    ) -> Result<Progress, IsoTpError<Tx::Error>> {
        // if cfg!(debug_assertions) {
        //     match &self.tx_state {
        //         TxState::Idle => println!("DEBUG poll_send state=Idle"),
        //         TxState::WaitingForFc { .. } => println!("DEBUG poll_send state=WaitingForFc"),
        //         TxState::Sending {
        //             st_min_deadline, ..
        //         } => println!(
        //             "DEBUG poll_send state=Sending deadline_set={}",
        //             st_min_deadline.is_some()
        //         ),
        //     };
        // }
        if payload.len() > self.cfg.max_payload_len {
            return Err(IsoTpError::Overflow);
        }

        let state = mem::replace(&mut self.tx_state, TxState::Idle);
        match state {
            TxState::Idle => self.start_send(payload, now),
            TxState::WaitingForFc { session, deadline } => {
                self.continue_wait_for_fc(session, deadline, now)
            }
            TxState::Sending {
                session,
                st_min_deadline,
            } => self.continue_send(session, st_min_deadline, now),
        }
    }

    /// Blocking send until completion or timeout.
    pub fn send(&mut self, payload: &[u8], timeout: Duration) -> Result<(), IsoTpError<Tx::Error>> {
        let start = self.clock.now();
        loop {
            let now = self.clock.now();
            if self.clock.elapsed(start) >= timeout {
                return Err(IsoTpError::Timeout(TimeoutKind::NAs));
            }
            match self.poll_send(payload, now)? {
                Progress::Completed => return Ok(()),
                Progress::WaitingForFlowControl | Progress::InFlight | Progress::WouldBlock => {
                    continue;
                }
            }
        }
    }

    /// Non-blocking receive step; delivers bytes when complete.
    pub fn poll_recv(
        &mut self,
        _now: C::Instant,
        deliver: &mut dyn FnMut(&[u8]),
    ) -> Result<Progress, IsoTpError<Tx::Error>> {
        loop {
            let frame = match self.rx.try_recv() {
                Ok(frame) => frame,
                Err(_) => {
                    return Ok(Progress::WouldBlock);
                }
            };

            if !id_matches(frame.id(), &self.cfg.rx_id) {
                continue;
            }

            let pdu = decode(frame.data()).map_err(|_| IsoTpError::InvalidFrame)?;
            if matches!(pdu, Pdu::FlowControl { .. }) {
                continue;
            }
            let outcome = match self.rx_machine.on_pdu(&self.cfg, pdu) {
                Ok(o) => o,
                Err(IsoTpError::Overflow) => {
                    let _ = self.send_overflow_fc();
                    return Err(IsoTpError::RxOverflow);
                }
                Err(IsoTpError::UnexpectedPdu) => continue,
                Err(IsoTpError::BadSequence) => return Err(IsoTpError::BadSequence),
                Err(IsoTpError::InvalidFrame) => return Err(IsoTpError::InvalidFrame),
                Err(IsoTpError::InvalidConfig) => return Err(IsoTpError::InvalidConfig),
                Err(IsoTpError::Timeout(kind)) => return Err(IsoTpError::Timeout(kind)),
                Err(IsoTpError::WouldBlock) => return Err(IsoTpError::WouldBlock),
                Err(IsoTpError::RxOverflow) => return Err(IsoTpError::RxOverflow),
                Err(IsoTpError::NotIdle) => return Err(IsoTpError::NotIdle),
                Err(IsoTpError::LinkError(_)) => return Err(IsoTpError::InvalidFrame),
            };

            match outcome {
                RxOutcome::None => return Ok(Progress::InFlight),
                RxOutcome::SendFlowControl {
                    status,
                    block_size,
                    st_min,
                } => {
                    self.send_flow_control(status, block_size, st_min)?;
                    return Ok(Progress::InFlight);
                }
                RxOutcome::Completed(_len) => {
                    let data = self.rx_machine.take_completed();
                    deliver(data);
                    return Ok(Progress::Completed);
                }
            }
        }
    }

    /// Blocking receive until a full payload arrives or timeout.
    pub fn recv(
        &mut self,
        timeout: Duration,
        deliver: &mut dyn FnMut(&[u8]),
    ) -> Result<(), IsoTpError<Tx::Error>> {
        let start = self.clock.now();
        loop {
            let now = self.clock.now();
            if self.clock.elapsed(start) >= timeout {
                return Err(IsoTpError::Timeout(TimeoutKind::NAr));
            }
            match self.poll_recv(now, deliver)? {
                Progress::Completed => return Ok(()),
                Progress::InFlight | Progress::WaitingForFlowControl | Progress::WouldBlock => {
                    continue;
                }
            }
        }
    }

    fn start_send(
        &mut self,
        payload: &[u8],
        now: C::Instant,
    ) -> Result<Progress, IsoTpError<Tx::Error>> {
        if payload.len() <= 7 {
            let pdu = Pdu::SingleFrame {
                len: payload.len() as u8,
                data: payload,
            };
            let frame = encode(self.cfg.tx_id, &pdu, self.cfg.padding)
                .map_err(|_| IsoTpError::InvalidFrame)?;
            self.tx.try_send(&frame).map_err(IsoTpError::LinkError)?;
            self.tx_state = TxState::Idle;
            return Ok(Progress::Completed);
        }

        #[cfg(not(any(feature = "alloc", feature = "std")))]
        {
            let _ = now;
            return Err(IsoTpError::InvalidConfig);
        }

        #[cfg(any(feature = "alloc", feature = "std"))]
        {
            let session = TxSession::new(payload.to_vec(), self.cfg.block_size, self.cfg.st_min);
            let len = session.payload.len();
            let chunk = session.payload.len().min(6);
            let pdu = Pdu::FirstFrame {
                len: len as u16,
                data: &session.payload[..chunk],
            };
            let frame = encode(self.cfg.tx_id, &pdu, self.cfg.padding)
                .map_err(|_| IsoTpError::InvalidFrame)?;
            self.tx.try_send(&frame).map_err(IsoTpError::LinkError)?;

            let mut session = session;
            session.offset = chunk;

            let deadline = self.clock.add(now, self.cfg.n_bs);
            self.tx_state = TxState::WaitingForFc { session, deadline };
            Ok(Progress::WaitingForFlowControl)
        }
    }

    fn continue_wait_for_fc(
        &mut self,
        mut session: TxSession,
        deadline: C::Instant,
        now: C::Instant,
    ) -> Result<Progress, IsoTpError<Tx::Error>> {
        if session.wait_count > self.cfg.wft_max {
            self.tx_state = TxState::Idle;
            return Err(IsoTpError::Timeout(TimeoutKind::NBs));
        }
        if now >= deadline {
            return Err(IsoTpError::Timeout(TimeoutKind::NBs));
        }
        loop {
            let frame = match self.rx.try_recv() {
                Ok(f) => f,
                Err(_) => {
                    self.tx_state = TxState::WaitingForFc { session, deadline };
                    return Ok(Progress::WaitingForFlowControl);
                }
            };
            if !id_matches(frame.id(), &self.cfg.rx_id) {
                continue;
            }
            let pdu = decode(frame.data()).map_err(|_| IsoTpError::InvalidFrame)?;
            match pdu {
                Pdu::FlowControl {
                    status,
                    block_size,
                    st_min,
                } => match status {
                    FlowStatus::ClearToSend => {
                        session.wait_count = 0;
                        let bs = if block_size == 0 {
                            self.cfg.block_size
                    } else {
                        block_size
                    };
                    session.block_size = bs;
                    session.block_remaining = bs;
                    session.st_min = st_min_to_duration(st_min).unwrap_or(self.cfg.st_min);
                    return self.continue_send(session, None, now);
                    }
                    FlowStatus::Wait => {
                        if cfg!(debug_assertions) {
                            println!("DEBUG wait_count before {}", session.wait_count);
                        }
                        session.wait_count = session.wait_count.saturating_add(1);
                        if session.wait_count > self.cfg.wft_max {
                            let deadline = self.clock.add(now, self.cfg.n_bs);
                            self.tx_state = TxState::WaitingForFc { session, deadline };
                            return Err(IsoTpError::Timeout(TimeoutKind::NBs));
                        }
                        let new_deadline = self.clock.add(now, self.cfg.n_bs);
                        self.tx_state = TxState::WaitingForFc {
                            session,
                            deadline: new_deadline,
                        };
                        return Ok(Progress::WaitingForFlowControl);
                    }
                    FlowStatus::Overflow => {
                        self.tx_state = TxState::Idle;
                        return Err(IsoTpError::Overflow);
                    }
                },
                _ => continue,
            }
        }
    }

    fn continue_send(
        &mut self,
        mut session: TxSession,
        st_min_deadline: Option<C::Instant>,
        now: C::Instant,
    ) -> Result<Progress, IsoTpError<Tx::Error>> {
        if let Some(deadline) = st_min_deadline {
            if now < deadline {
                self.tx_state = TxState::Sending {
                    session,
                    st_min_deadline: Some(deadline),
                };
                return Ok(Progress::WouldBlock);
            }
        }

        if session.offset >= session.payload.len() {
            self.tx_state = TxState::Idle;
            return Ok(Progress::Completed);
        }

        let remaining = session.payload.len() - session.offset;
        let chunk = remaining.min(7);
        let data = &session.payload[session.offset..session.offset + chunk];
        let pdu = Pdu::ConsecutiveFrame {
            sn: session.next_sn & 0x0F,
            data,
        };
        let frame =
            encode(self.cfg.tx_id, &pdu, self.cfg.padding).map_err(|_| IsoTpError::InvalidFrame)?;
        self.tx.try_send(&frame).map_err(IsoTpError::LinkError)?;

        session.offset += chunk;
        session.next_sn = (session.next_sn + 1) & 0x0F;

        if session.offset >= session.payload.len() {
            self.tx_state = TxState::Idle;
            return Ok(Progress::Completed);
        }

        if session.block_size > 0 {
            session.block_remaining = session.block_remaining.saturating_sub(1);
            if session.block_remaining == 0 {
                session.block_remaining = session.block_size;
                let deadline = self.clock.add(now, self.cfg.n_bs);
                self.tx_state = TxState::WaitingForFc { session, deadline };
                return Ok(Progress::WaitingForFlowControl);
            }
        }

        let next_deadline = if session.st_min > Duration::from_millis(0) {
            Some(self.clock.add(now, session.st_min))
        } else {
            None
        };
        self.tx_state = TxState::Sending {
            session,
            st_min_deadline: next_deadline,
        };
        Ok(Progress::InFlight)
    }

    fn send_flow_control(
        &mut self,
        status: FlowStatus,
        block_size: u8,
        st_min: u8,
    ) -> Result<(), IsoTpError<Tx::Error>> {
        let fc = Pdu::FlowControl {
            status,
            block_size,
            st_min,
        };
        let frame =
            encode(self.cfg.tx_id, &fc, self.cfg.padding).map_err(|_| IsoTpError::InvalidFrame)?;
        self.tx.try_send(&frame).map_err(IsoTpError::LinkError)?;
        Ok(())
    }

    fn send_overflow_fc(&mut self) -> Result<(), IsoTpError<Tx::Error>> {
        self.send_flow_control(FlowStatus::Overflow, 0, duration_to_st_min(self.cfg.st_min))
    }
}

fn id_matches(actual: embedded_can::Id, expected: &Id) -> bool {
    match (actual, expected) {
        (embedded_can::Id::Standard(a), Id::Standard(b)) => a == *b,
        (embedded_can::Id::Extended(a), Id::Extended(b)) => a == *b,
        _ => false,
    }
}

fn build_buffer<'a>(
    len: usize,
    buffer: Option<&'a mut [u8]>,
) -> Result<RxBuffer<'a>, IsoTpError<()>> {
    if let Some(buf) = buffer {
        if buf.len() < len {
            return Err(IsoTpError::InvalidConfig);
        }
        return Ok(RxBuffer::Borrowed(&mut buf[..len]));
    }
    #[cfg(any(feature = "alloc", feature = "std"))]
    {
        let mut buf = Vec::new();
        buf.resize(len, 0);
        return Ok(RxBuffer::Owned(buf));
    }
    #[cfg(not(any(feature = "alloc", feature = "std")))]
    {
        let _ = len;
        Err(IsoTpError::InvalidConfig)
    }
}

#[cfg(feature = "std")]
impl<'a, Tx, Rx, F> IsoTpNode<'a, Tx, Rx, F, StdClock>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
{
    /// Convenience constructor using `StdClock` and an owned buffer.
    pub fn with_std_clock(tx: Tx, rx: Rx, cfg: IsoTpConfig) -> Result<Self, IsoTpError<()>> {
        IsoTpNode::with_clock(tx, rx, cfg, StdClock, None)
    }
}
