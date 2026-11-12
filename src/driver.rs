//! Independently driven, fixed-storage ISO-TP application ports.
//!
//! The driver owns reassembly and accepted TX leases. Application receive/send
//! cancellation never drops protocol state. `send` reports acceptance into this
//! state, not transaction completion; observe [`DriverHooks::transmitted`] for
//! the latter. No automatic replay is performed.
//!
//! The lower CAN receiver's `recv` must be cancellation safe: dropping a pending
//! receive must preserve the next frame. Raw frame sends are retained until
//! completion (or session shutdown), rather than cancelled on every RX event.
use crate::{Clock, IsoTpConfig, IsoTpDemux, IsoTpError, Progress, RxStorage};
use can_isotp_interface::{Deadline, Destination, ReceivedPayload, RecvMeta, TransmitError};
use can_isotp_queue::{
    QueueError,
    embassy::{RequestLease, RxEvent},
};
use core::{
    cell::RefCell,
    future::{Future, pending},
    time::Duration,
};
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_can::Frame;
use embedded_can_interface::{AsyncRxFrameIo, AsyncTxFrameIo, RxFrameIo, TxFrameIo};

/// Fixed-storage application ports shared with the queue adapter.
pub type DriverResources<RM, E, D, const MAX: usize, const N: usize, const EVENTS: usize> =
    can_isotp_queue::embassy::QueueResources<RM, IsoTpError<E>, IsoTpError<E>, D, MAX, N, EVENTS>;
/// Cloneable addressed application writer.
pub type DriverTx<'a, RM, E, D, const MAX: usize, const N: usize, const EVENTS: usize> =
    can_isotp_queue::embassy::QueueTx<'a, RM, IsoTpError<E>, IsoTpError<E>, D, MAX, N, EVENTS>;
/// Exclusively owned addressed application reader.
pub type DriverRx<'a, RM, E, D, const MAX: usize, const N: usize, const EVENTS: usize> =
    can_isotp_queue::embassy::QueueRx<'a, RM, IsoTpError<E>, IsoTpError<E>, D, MAX, N, EVENTS>;
/// Protocol driver's application admission and delivery ports.
pub type DriverPorts<'a, RM, E, D, const MAX: usize, const N: usize, const EVENTS: usize> =
    can_isotp_queue::embassy::DriverPorts<'a, RM, IsoTpError<E>, IsoTpError<E>, D, MAX, N, EVENTS>;

/// Completion is observational and independent of the accepted command caller.
pub trait DriverHooks<E> {
    fn transmitted(&self, _destination: Destination, _result: Result<(), IsoTpError<E>>) {}
}
impl<E> DriverHooks<E> for () {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DriverStop {
    Closed,
    Shutdown,
}

/// Internal frame staging is bounded: one RX-generated FC plus one TX frame.
struct Staging<F> {
    input: Option<F>,
    output: [Option<F>; 2],
}
struct StagedTx<'a, F>(&'a RefCell<Staging<F>>);
struct StagedRx<'a, F>(&'a RefCell<Staging<F>>);
impl<F: Clone> TxFrameIo for StagedTx<'_, F> {
    type Frame = F;
    type Error = ();
    fn try_send(&mut self, frame: &F) -> Result<(), ()> {
        let mut stage = self.0.borrow_mut();
        let slot = stage
            .output
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(())?;
        *slot = Some(frame.clone());
        Ok(())
    }
    fn send(&mut self, frame: &F) -> Result<(), ()> {
        self.try_send(frame)
    }
    fn send_timeout(&mut self, frame: &F, _: Duration) -> Result<(), ()> {
        self.try_send(frame)
    }
}
impl<F> RxFrameIo for StagedRx<'_, F> {
    type Frame = F;
    type Error = ();
    fn try_recv(&mut self) -> Result<F, ()> {
        self.0.borrow_mut().input.take().ok_or(())
    }
    fn recv(&mut self) -> Result<F, ()> {
        self.try_recv()
    }
    fn recv_timeout(&mut self, _: Duration) -> Result<F, ()> {
        self.try_recv()
    }
    fn wait_not_empty(&mut self) -> Result<(), ()> {
        if self.0.borrow().input.is_some() {
            Ok(())
        } else {
            Err(())
        }
    }
}
fn protocol_error<E>(error: IsoTpError<()>) -> IsoTpError<E> {
    match error {
        IsoTpError::Timeout(kind) => IsoTpError::Timeout(kind),
        IsoTpError::UnexpectedPdu => IsoTpError::UnexpectedPdu,
        IsoTpError::BadSequence => IsoTpError::BadSequence,
        IsoTpError::Overflow => IsoTpError::Overflow,
        IsoTpError::InvalidFrame => IsoTpError::InvalidFrame,
        IsoTpError::InvalidConfig => IsoTpError::InvalidConfig,
        IsoTpError::WouldBlock => IsoTpError::WouldBlock,
        IsoTpError::RxOverflow => IsoTpError::RxOverflow,
        IsoTpError::NotIdle => IsoTpError::NotIdle,
        // Exhausting internal frame staging is a driver invariant failure.
        IsoTpError::LinkError(()) => IsoTpError::InvalidConfig,
    }
}

/// Drive the existing demux state machines from actual input, request admission,
/// and the nearest active protocol timer. Resources may be split only once.
///
/// `C::Instant` also implements the canonical deadline contract, ensuring timers
/// and application admission use the same clock. `MAX` must cover configured PDUs.
/// RX backpressure retains one complete event and pauses new frame reads; TX
/// timers and shutdown remain live. Hooks must be short and nonblocking.
#[allow(clippy::too_many_arguments)]
pub async fn run_driver<
    'a,
    Tx,
    Rx,
    F,
    C,
    RM,
    H,
    S,
    const PEERS: usize,
    const MAX: usize,
    const N: usize,
    const EVENTS: usize,
>(
    mut tx: Tx,
    mut rx: Rx,
    clock: C,
    config: IsoTpConfig,
    local: u8,
    functional: Option<u8>,
    storages: [RxStorage<'a>; PEERS],
    ports: DriverPorts<'_, RM, Tx::Error, C::Instant, MAX, N, EVENTS>,
    hooks: H,
    shutdown: S,
) -> Result<DriverStop, IsoTpError<Tx::Error>>
where
    Tx: AsyncTxFrameIo<Frame = F>,
    Rx: AsyncRxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame + Clone,
    C: Clock + Clone,
    C::Instant: Deadline,
    RM: RawMutex,
    H: DriverHooks<Tx::Error>,
    S: Future<Output = ()>,
{
    if MAX < config.max_payload_len || PEERS == 0 {
        return Err(IsoTpError::InvalidConfig);
    }
    let staging = RefCell::new(Staging {
        input: None,
        output: [None, None],
    });
    let mut demux = IsoTpDemux::new(
        StagedTx(&staging),
        StagedRx(&staging),
        config.clone(),
        clock.clone(),
        local,
        storages,
    )
    .map_err(protocol_error)?;
    if let Some(address) = functional {
        demux = demux.with_functional_addr(address);
    }
    let mut active: Option<RequestLease<'_, RM, _, _, _, MAX, N, EVENTS>> = None;
    let mut event = None;
    let mut completed = None;
    let mut pending_request = None;
    let drive = async {
        let mut tx_open = true;
        loop {
            if ports.is_closed() || (!tx_open && !ports.receive_open() && active.is_none()) {
                return Ok(DriverStop::Closed);
            }
            if !ports.receive_open() {
                event = None;
            }
            demux.tick();
            embassy_futures::yield_now().await;
            if let Some(pending) = event.take() {
                event = ports.try_event(pending).err();
            }
            if event.is_none() && demux.take_receive_timeout().is_some() {
                event = Some(RxEvent::Rejected(IsoTpError::Timeout(
                    crate::TimeoutKind::NBr,
                )));
            }
            if event.is_none() {
                let result = demux.poll_recv(clock.now(), &mut |reply_to, payload| {
                    let mut bytes = [0; MAX];
                    bytes[..payload.len()].copy_from_slice(payload);
                    event = Some(RxEvent::Payload(
                        ReceivedPayload {
                            meta: RecvMeta { reply_to },
                            len: payload.len(),
                        },
                        bytes,
                    ));
                });
                if let Err(error) = result {
                    event = Some(RxEvent::Rejected(protocol_error(error)));
                }
            }
            if active.is_none() && tx_open {
                while let Some(request) = pending_request.take().or_else(|| ports.try_request()) {
                    if request.is_cancelled() {
                        continue;
                    }
                    let invalid = request.with_request(|destination, payload, _| {
                        if payload.is_empty() || payload.len() > config.max_payload_len {
                            Some(IsoTpError::Overflow)
                        } else if matches!(destination, Destination::Functional(_))
                            && payload.len() > config.max_single_frame_payload()
                        {
                            Some(IsoTpError::InvalidFrame)
                        } else {
                            None
                        }
                    });
                    if let Some(error) = invalid {
                        request.complete(Err(TransmitError::Rejected(QueueError::Backend(error))));
                        continue;
                    }
                    // Deadline is checked at the acceptance boundary, after validation.
                    if request.is_cancelled() {
                        continue;
                    }
                    request.accept();
                    active = Some(request);
                    break;
                }
            }
            if let Some(request) = &active
                && completed.is_none()
            {
                let result = request.with_request(|destination, payload, _| match destination {
                    Destination::Physical(peer) => demux.poll_send_to(peer, payload, clock.now()),
                    Destination::Functional(peer) => demux
                        .send_functional_to(peer, payload, config.n_as)
                        .map(|()| Progress::Completed),
                });
                match result {
                    Ok(Progress::Completed) => completed = Some(Ok(())),
                    Err(error) => {
                        request.with_request(|destination, _, _| {
                            if let Destination::Physical(peer) = destination {
                                demux.reset_send(peer)
                            }
                        });
                        completed = Some(Err(protocol_error(error)));
                    }
                    _ => {}
                }
            }
            // Drain staged frame sends without dropping their future for RX/timers.
            for index in 0..2 {
                let frame = staging.borrow_mut().output[index].take();
                if let Some(frame) = frame {
                    let sending = can_isotp_interface::until(
                        clock.add(clock.now(), config.n_as),
                        tx.send_timeout(&frame, config.n_as),
                    );
                    let mut sending = core::pin::pin!(sending);
                    // Keep one incoming raw frame while output is blocked. The TX
                    // future remains pinned across this event; never replay a frame.
                    loop {
                        let receive = async {
                            if event.is_none() && staging.borrow().input.is_none() {
                                rx.recv().await
                            } else {
                                pending().await
                            }
                        };
                        match select(sending.as_mut(), receive).await {
                            Either::First(result) => {
                                result
                                    .map_err(|_| IsoTpError::Timeout(crate::TimeoutKind::NAs))?
                                    .map_err(IsoTpError::LinkError)?;
                                break;
                            }
                            Either::Second(result) => {
                                staging.borrow_mut().input =
                                    Some(result.map_err(IsoTpError::LinkError)?)
                            }
                        }
                    }
                    if frame
                        .data()
                        .get(usize::from(config.tx_addr.is_some()))
                        .is_some_and(|pci| matches!(pci >> 4, 1 | 2))
                        && let Some(address) = can_uds::uds29::decode_id(frame.id())
                    {
                        demux.on_tx_frame_accepted(address.target);
                    }
                }
            }
            if let Some(result) = completed.take() {
                let request = active.take().expect("completion has active request");
                request.with_request(|destination, _, _| hooks.transmitted(destination, result));
            }
            // A Sending state whose separation deadline is ready requires immediate
            // progress; waiting-for-FC and RX reassembly wait for actual active timers.
            if !ports.receive_open() {
                event = None;
            }
            if staging.borrow().input.is_some() {
                continue;
            }
            let wake = demux.next_wakeup();
            if wake
                .as_ref()
                .is_some_and(|deadline| deadline.remaining().is_zero())
            {
                continue;
            }
            let timer = async {
                match wake {
                    Some(deadline) => deadline.wait().await,
                    None => pending().await,
                }
            };
            let receive = async {
                if event.is_none() {
                    rx.recv().await
                } else {
                    pending().await
                }
            };
            let admission = async {
                if active.is_none() && tx_open {
                    ports.next_request().await
                } else {
                    pending().await
                }
            };
            let progress = select3(receive, timer, admission);
            let ready = async {
                if event.is_some() {
                    let _ = ports.event_ready().await;
                } else {
                    pending().await
                }
            };
            match select(progress, ready).await {
                Either::First(Either3::First(result)) => {
                    staging.borrow_mut().input = Some(result.map_err(IsoTpError::LinkError)?)
                }
                Either::First(Either3::Second(())) | Either::Second(()) => {}
                Either::First(Either3::Third(request)) => {
                    let Some(request) = request else {
                        tx_open = false;
                        continue;
                    };
                    pending_request = Some(request);
                }
            }
        }
    };
    match select3(ports.closed(), shutdown, drive).await {
        Either3::First(()) => Ok(DriverStop::Closed),
        Either3::Second(()) => Ok(DriverStop::Shutdown),
        Either3::Third(result) => result,
    }
}
