//! Implementations of the `can-isotp-interface` traits for this crate.

#[cfg(feature = "isotp-interface")]
use can_isotp_interface::{
    IsoTpEndpoint, IsoTpRxFlowControlConfig, RecvControl, RecvError, RecvMeta, RecvStatus,
    RxFlowControl as IfaceRxFlowControl, SendError,
};

#[cfg(feature = "isotp-interface")]
use crate::{IsoTpAsyncNode, IsoTpError, IsoTpNode, TimeoutKind};

#[cfg(feature = "isotp-interface")]
use core::time::Duration;

#[cfg(feature = "isotp-interface")]
use embedded_can::Frame;

#[cfg(feature = "isotp-interface")]
use embedded_can_interface::{AsyncRxFrameIo, AsyncTxFrameIo, RxFrameIo, TxFrameIo};

#[cfg(feature = "isotp-interface")]
use crate::timer::Clock;

#[cfg(feature = "isotp-interface")]
impl<'a, Tx, Rx, F, C> IsoTpRxFlowControlConfig for IsoTpNode<'a, Tx, Rx, F, C>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    type Error = IsoTpError<Tx::Error>;

    fn set_rx_flow_control(&mut self, fc: IfaceRxFlowControl) -> Result<(), Self::Error> {
        self.set_rx_flow_control(crate::RxFlowControl {
            block_size: fc.block_size,
            st_min: fc.st_min,
        });
        Ok(())
    }
}

#[cfg(feature = "isotp-interface")]
impl<'a, Tx, Rx, F, C> IsoTpRxFlowControlConfig for IsoTpAsyncNode<'a, Tx, Rx, F, C>
where
    Tx: AsyncTxFrameIo<Frame = F>,
    Rx: AsyncRxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    type Error = IsoTpError<Tx::Error>;

    fn set_rx_flow_control(&mut self, fc: IfaceRxFlowControl) -> Result<(), Self::Error> {
        self.set_rx_flow_control(crate::RxFlowControl {
            block_size: fc.block_size,
            st_min: fc.st_min,
        });
        Ok(())
    }
}

#[cfg(feature = "isotp-interface")]
impl<'a, Tx, Rx, F, C> IsoTpEndpoint for IsoTpNode<'a, Tx, Rx, F, C>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    type Error = IsoTpError<Tx::Error>;

    fn send_to(
        &mut self,
        _to: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), SendError<Self::Error>> {
        IsoTpNode::send(self, payload, timeout).map_err(|e| match e {
            IsoTpError::Timeout(TimeoutKind::NAs) => SendError::Timeout,
            other => SendError::Backend(other),
        })
    }

    fn send_functional_to(
        &mut self,
        _functional_to: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), SendError<Self::Error>> {
        IsoTpNode::send(self, payload, timeout).map_err(|e| match e {
            IsoTpError::Timeout(TimeoutKind::NAs) => SendError::Timeout,
            other => SendError::Backend(other),
        })
    }

    fn recv_one<Cb>(
        &mut self,
        timeout: Duration,
        mut on_payload: Cb,
    ) -> Result<RecvStatus, RecvError<Self::Error>>
    where
        Cb: FnMut(RecvMeta, &[u8]) -> Result<RecvControl, Self::Error>,
    {
        let mut delivered = false;
        let mut cb_err: Option<Self::Error> = None;

        let res = IsoTpNode::recv(self, timeout, &mut |payload| {
            delivered = true;
            if cb_err.is_none()
                && let Err(e) = on_payload(RecvMeta { reply_to: 0 }, payload)
            {
                cb_err = Some(e);
            }
        });

        if let Some(e) = cb_err {
            return Err(RecvError::Backend(e));
        }

        match res {
            Ok(()) => Ok(if delivered {
                RecvStatus::DeliveredOne
            } else {
                RecvStatus::TimedOut
            }),
            Err(IsoTpError::Timeout(_)) => Ok(RecvStatus::TimedOut),
            Err(e) => Err(RecvError::Backend(e)),
        }
    }
}

#[cfg(all(feature = "isotp-interface", feature = "uds"))]
impl<'a, Tx, Rx, F, C, const MAX: usize> IsoTpEndpoint
    for crate::demux::IsoTpDemux<'a, Tx, Rx, F, C, MAX>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    type Error = IsoTpError<Tx::Error>;

    fn send_to(
        &mut self,
        to: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), SendError<Self::Error>> {
        crate::demux::IsoTpDemux::send_to(self, to, payload, timeout).map_err(|e| match e {
            IsoTpError::Timeout(TimeoutKind::NAs) => SendError::Timeout,
            other => SendError::Backend(other),
        })
    }

    fn send_functional_to(
        &mut self,
        functional_to: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), SendError<Self::Error>> {
        crate::demux::IsoTpDemux::send_functional_to(self, functional_to, payload, timeout).map_err(
            |e| match e {
                IsoTpError::Timeout(TimeoutKind::NAs) => SendError::Timeout,
                other => SendError::Backend(other),
            },
        )
    }

    fn recv_one<Cb>(
        &mut self,
        timeout: Duration,
        mut on_payload: Cb,
    ) -> Result<RecvStatus, RecvError<Self::Error>>
    where
        Cb: FnMut(RecvMeta, &[u8]) -> Result<RecvControl, Self::Error>,
    {
        let mut delivered = false;
        let mut cb_err: Option<Self::Error> = None;

        let res = crate::demux::IsoTpDemux::recv(self, timeout, &mut |reply_to, payload| {
            delivered = true;
            if cb_err.is_none()
                && let Err(e) = on_payload(RecvMeta { reply_to }, payload)
            {
                cb_err = Some(e);
            }
        });

        if let Some(e) = cb_err {
            return Err(RecvError::Backend(e));
        }

        match res {
            Ok(()) => Ok(if delivered {
                RecvStatus::DeliveredOne
            } else {
                RecvStatus::TimedOut
            }),
            Err(IsoTpError::Timeout(_)) => Ok(RecvStatus::TimedOut),
            Err(e) => Err(RecvError::Backend(e)),
        }
    }
}

#[cfg(all(feature = "isotp-interface", feature = "uds"))]
impl<'a, Tx, Rx, F, C, const MAX: usize> IsoTpRxFlowControlConfig
    for crate::demux::IsoTpDemux<'a, Tx, Rx, F, C, MAX>
where
    Tx: TxFrameIo<Frame = F>,
    Rx: RxFrameIo<Frame = F, Error = Tx::Error>,
    F: Frame,
    C: Clock,
{
    type Error = IsoTpError<Tx::Error>;

    fn set_rx_flow_control(&mut self, fc: IfaceRxFlowControl) -> Result<(), Self::Error> {
        self.set_rx_flow_control(crate::RxFlowControl {
            block_size: fc.block_size,
            st_min: fc.st_min,
        });
        Ok(())
    }
}
