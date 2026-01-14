//! Async ISO-TP node implementation.

use core::time::Duration;

use embedded_can::Frame;
use embedded_can_interface::{AsyncRxFrameIo, AsyncTxFrameIo};

use crate::async_io::AsyncRuntime;
use crate::errors::{IsoTpError, TimeoutKind};
use crate::pdu::{FlowStatus, Pdu, decode, duration_to_st_min, encode, st_min_to_duration};
use crate::rx::{RxMachine, RxOutcome};
use crate::timer::Clock;
use crate::{IsoTpConfig, id_matches};

/// Async ISO-TP endpoint backed by async transmit/receive halves and a clock.
pub struct IsoTpAsyncNode<'a, Tx, Rx, F, C>
where
    Tx: AsyncTxFrameIo<Frame = F>,
    Rx: AsyncRxFrameIo<Frame = F, Error = Tx::Error>,
    C: Clock,
{
    tx: Tx,
    rx: Rx,
    cfg: IsoTpConfig,
    clock: C,
    rx_machine: RxMachine<'a>,
}

impl<'a, Tx, Rx, F, C> IsoTpAsyncNode<'a, Tx, Rx, F, C>
where
    Tx: AsyncTxFrameIo<Frame = F>,
    Rx: AsyncRxFrameIo<Frame = F, Error = Tx::Error>,
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
        let rx_buffer = crate::build_buffer(cfg.rx_buffer_len, buffer)?;
        Ok(Self {
            tx,
            rx,
            cfg,
            clock,
            rx_machine: RxMachine::new(rx_buffer),
        })
    }

    /// Blocking-by-await send until completion or timeout.
    pub async fn send<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), IsoTpError<Tx::Error>> {
        if payload.len() > self.cfg.max_payload_len {
            return Err(IsoTpError::Overflow);
        }

        let start = self.clock.now();
        if self.clock.elapsed(start) >= timeout {
            return Err(IsoTpError::Timeout(TimeoutKind::NAs));
        }

        if payload.len() <= 7 {
            let pdu = Pdu::SingleFrame {
                len: payload.len() as u8,
                data: payload,
            };
            let frame =
                encode(self.cfg.tx_id, &pdu, self.cfg.padding).map_err(|_| IsoTpError::InvalidFrame)?;
            self.send_frame(rt, start, timeout, TimeoutKind::NAs, &frame)
                .await?;
            return Ok(());
        }

        let mut offset = payload.len().min(6);
        let mut next_sn: u8 = 1;
        let wait_count: u8 = 0;

        let pdu = Pdu::FirstFrame {
            len: payload.len() as u16,
            data: &payload[..offset],
        };
        let frame =
            encode(self.cfg.tx_id, &pdu, self.cfg.padding).map_err(|_| IsoTpError::InvalidFrame)?;
        self.send_frame(rt, start, timeout, TimeoutKind::NAs, &frame)
            .await?;

        let fc_start = self.clock.now();
        let (mut block_size, mut st_min, mut wait_count) =
            self.wait_for_flow_control(rt, start, timeout, fc_start, wait_count)
                .await?;
        let mut block_remaining = block_size;

        let mut last_cf_sent: Option<C::Instant> = None;
        while offset < payload.len() {
            if block_size > 0 && block_remaining == 0 {
                let fc_start = self.clock.now();
                let (new_bs, new_st_min, new_wait_count) =
                    self.wait_for_flow_control(rt, start, timeout, fc_start, wait_count)
                        .await?;
                block_size = new_bs;
                block_remaining = new_bs;
                st_min = new_st_min;
                wait_count = new_wait_count;
                continue;
            }

            if let Some(sent_at) = last_cf_sent {
                let elapsed = self.clock.elapsed(sent_at);
                if elapsed < st_min {
                    let wait_for = st_min - elapsed;
                    sleep_or_timeout(&self.clock, rt, start, timeout, TimeoutKind::NAs, wait_for)
                        .await?;
                }
            }

            let remaining = payload.len() - offset;
            let chunk = remaining.min(7);
            let pdu = Pdu::ConsecutiveFrame {
                sn: next_sn & 0x0F,
                data: &payload[offset..offset + chunk],
            };
            let frame =
                encode(self.cfg.tx_id, &pdu, self.cfg.padding).map_err(|_| IsoTpError::InvalidFrame)?;
            self.send_frame(rt, start, timeout, TimeoutKind::NAs, &frame)
                .await?;

            last_cf_sent = Some(self.clock.now());
            offset += chunk;
            next_sn = (next_sn + 1) & 0x0F;

            if block_size > 0 {
                block_remaining = block_remaining.saturating_sub(1);
            }
        }

        Ok(())
    }

    /// Blocking-by-await receive until a full payload arrives or timeout.
    pub async fn recv<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        timeout: Duration,
        deliver: &mut dyn FnMut(&[u8]),
    ) -> Result<(), IsoTpError<Tx::Error>> {
        let start = self.clock.now();

        loop {
            let frame = self.recv_frame(rt, start, timeout, TimeoutKind::NAr).await?;

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
                    let _ = self.send_overflow_fc(rt, start, timeout).await;
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
                RxOutcome::None => continue,
                RxOutcome::SendFlowControl {
                    status,
                    block_size,
                    st_min,
                } => {
                    self.send_flow_control(rt, start, timeout, status, block_size, st_min)
                        .await?;
                }
                RxOutcome::Completed(_len) => {
                    let data = self.rx_machine.take_completed();
                    deliver(data);
                    return Ok(());
                }
            }
        }
    }

    async fn wait_for_flow_control<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        global_start: C::Instant,
        global_timeout: Duration,
        mut fc_start: C::Instant,
        mut wait_count: u8,
    ) -> Result<(u8, Duration, u8), IsoTpError<Tx::Error>> {
        loop {
            if self.clock.elapsed(fc_start) >= self.cfg.n_bs {
                return Err(IsoTpError::Timeout(TimeoutKind::NBs));
            }

            let fc_remaining = self.cfg.n_bs - self.clock.elapsed(fc_start);
            let global_remaining = remaining(global_timeout, self.clock.elapsed(global_start))
                .ok_or(IsoTpError::Timeout(TimeoutKind::NAs))?;
            let wait_for = fc_remaining.min(global_remaining);

            let frame = match rt.timeout(wait_for, self.rx.recv()).await {
                Ok(Ok(f)) => f,
                Ok(Err(err)) => return Err(IsoTpError::LinkError(err)),
                Err(_) => {
                    if global_remaining <= fc_remaining {
                        return Err(IsoTpError::Timeout(TimeoutKind::NAs));
                    }
                    return Err(IsoTpError::Timeout(TimeoutKind::NBs));
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
                        let bs = if block_size == 0 {
                            self.cfg.block_size
                        } else {
                            block_size
                        };
                        let st_min = st_min_to_duration(st_min).unwrap_or(self.cfg.st_min);
                        return Ok((bs, st_min, 0));
                    }
                    FlowStatus::Wait => {
                        wait_count = wait_count.saturating_add(1);
                        if wait_count > self.cfg.wft_max {
                            return Err(IsoTpError::Timeout(TimeoutKind::NBs));
                        }
                        fc_start = self.clock.now();
                        continue;
                    }
                    FlowStatus::Overflow => return Err(IsoTpError::Overflow),
                },
                _ => continue,
            }
        }
    }

    async fn send_flow_control<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        start: C::Instant,
        timeout: Duration,
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
        self.send_frame(rt, start, timeout, TimeoutKind::NAs, &frame)
            .await?;
        Ok(())
    }

    async fn send_overflow_fc<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        start: C::Instant,
        timeout: Duration,
    ) -> Result<(), IsoTpError<Tx::Error>> {
        self.send_flow_control(
            rt,
            start,
            timeout,
            FlowStatus::Overflow,
            0,
            duration_to_st_min(self.cfg.st_min),
        )
        .await
    }

    async fn send_frame<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        start: C::Instant,
        timeout: Duration,
        kind: TimeoutKind,
        frame: &F,
    ) -> Result<(), IsoTpError<Tx::Error>> {
        let remaining =
            remaining(timeout, self.clock.elapsed(start)).ok_or(IsoTpError::Timeout(kind))?;
        match rt.timeout(remaining, self.tx.send(frame)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(IsoTpError::LinkError(err)),
            Err(_) => Err(IsoTpError::Timeout(kind)),
        }
    }

    async fn recv_frame<R: AsyncRuntime>(
        &mut self,
        rt: &R,
        start: C::Instant,
        timeout: Duration,
        kind: TimeoutKind,
    ) -> Result<F, IsoTpError<Tx::Error>> {
        let remaining =
            remaining(timeout, self.clock.elapsed(start)).ok_or(IsoTpError::Timeout(kind))?;
        match rt.timeout(remaining, self.rx.recv()).await {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(err)) => Err(IsoTpError::LinkError(err)),
            Err(_) => Err(IsoTpError::Timeout(kind)),
        }
    }
}

fn remaining(timeout: Duration, elapsed: Duration) -> Option<Duration> {
    timeout.checked_sub(elapsed)
}

async fn sleep_or_timeout<C: Clock, R: AsyncRuntime, E>(
    clock: &C,
    rt: &R,
    start: C::Instant,
    timeout: Duration,
    kind: TimeoutKind,
    duration: Duration,
) -> Result<(), IsoTpError<E>> {
    let remaining = remaining(timeout, clock.elapsed(start)).ok_or(IsoTpError::Timeout(kind))?;
    let wait_for = duration.min(remaining);
    let sleep_fut = rt.sleep(wait_for);
    match rt.timeout(wait_for, sleep_fut).await {
        Ok(()) => Ok(()),
        Err(_) => Err(IsoTpError::Timeout(kind)),
    }
}

#[cfg(feature = "std")]
impl<'a, Tx, Rx, F> IsoTpAsyncNode<'a, Tx, Rx, F, crate::StdClock>
where
    Tx: AsyncTxFrameIo<Frame = F>,
    Rx: AsyncRxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
{
    /// Convenience constructor using `StdClock` and an owned buffer.
    pub fn with_std_clock(tx: Tx, rx: Rx, cfg: IsoTpConfig) -> Result<Self, IsoTpError<()>> {
        IsoTpAsyncNode::with_clock(tx, rx, cfg, crate::StdClock, None)
    }
}
