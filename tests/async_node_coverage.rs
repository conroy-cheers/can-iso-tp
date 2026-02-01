use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use embedded_can::StandardId;
use embedded_can_interface::Id;
use embedded_can_interface::{AsyncRxFrameIo, AsyncTxFrameIo};
use embedded_can_mock::MockFrame;
use iso_tp::{AsyncRuntime, IsoTpAsyncNode, IsoTpConfig, IsoTpError, TimeoutKind};

#[derive(Default)]
struct NoTx;

#[derive(Default)]
struct NoRx;

impl AsyncTxFrameIo for NoTx {
    type Frame = MockFrame;
    type Error = ();

    async fn send(&mut self, _frame: &Self::Frame) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn send_timeout(
        &mut self,
        frame: &Self::Frame,
        _timeout: Duration,
    ) -> Result<(), Self::Error> {
        self.send(frame).await
    }
}

impl AsyncRxFrameIo for NoRx {
    type Frame = MockFrame;
    type Error = ();

    async fn recv(&mut self) -> Result<Self::Frame, Self::Error> {
        Err(())
    }

    async fn recv_timeout(&mut self, _timeout: Duration) -> Result<Self::Frame, Self::Error> {
        Err(())
    }

    async fn wait_not_empty(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct UnusedRt;

impl AsyncRuntime for UnusedRt {
    type TimeoutError = ();

    type Sleep<'a>
        = core::future::Ready<()>
    where
        Self: 'a;

    fn sleep<'a>(&'a self, _duration: Duration) -> Self::Sleep<'a> {
        panic!("runtime should not be used in these tests");
    }

    type Timeout<'a, F>
        = Pin<Box<dyn Future<Output = Result<F::Output, Self::TimeoutError>> + 'a>>
    where
        Self: 'a,
        F: Future + 'a;

    fn timeout<'a, F>(&'a self, _duration: Duration, _future: F) -> Self::Timeout<'a, F>
    where
        F: Future + 'a,
    {
        panic!("runtime should not be used in these tests");
    }
}

fn cfg(max_payload_len: usize) -> IsoTpConfig {
    IsoTpConfig {
        tx_id: Id::Standard(StandardId::new(0x700).unwrap()),
        rx_id: Id::Standard(StandardId::new(0x701).unwrap()),
        max_payload_len,
        rx_buffer_len: 64,
        ..IsoTpConfig::default()
    }
}

#[tokio::test]
async fn async_send_rejects_oversized_payload_early() {
    let mut node = IsoTpAsyncNode::with_std_clock(NoTx, NoRx, cfg(1)).unwrap();
    let rt = UnusedRt;
    let payload = [0u8; 2];
    let err = node
        .send(&rt, &payload, Duration::from_millis(10))
        .await
        .unwrap_err();
    assert!(matches!(err, IsoTpError::Overflow));
}

#[tokio::test]
async fn async_send_times_out_immediately_when_timeout_is_zero() {
    let mut node = IsoTpAsyncNode::with_std_clock(NoTx, NoRx, cfg(8)).unwrap();
    let rt = UnusedRt;
    let payload = [0u8; 1];
    let err = node
        .send(&rt, &payload, Duration::from_millis(0))
        .await
        .unwrap_err();
    assert!(matches!(err, IsoTpError::Timeout(TimeoutKind::NAs)));
}
