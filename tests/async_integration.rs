use core::time::Duration;
use std::sync::Arc;

use embedded_can_interface::Id;
use embedded_can_mock::MockFrame;
use iso_tp::{AsyncRuntime, IsoTpAsyncNode, IsoTpConfig};
use tokio::sync::{Mutex, mpsc};

#[derive(Debug)]
enum TokioCanError {
    Closed,
}

struct TokioBus {
    peers: Mutex<Vec<mpsc::UnboundedSender<MockFrame>>>,
}

impl TokioBus {
    fn new() -> Self {
        Self {
            peers: Mutex::new(Vec::new()),
        }
    }

    async fn add_interface(self: &Arc<Self>) -> (TokioTx, TokioRx) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.peers.lock().await.push(tx);
        (
            TokioTx { bus: self.clone() },
            TokioRx { rx },
        )
    }

    async fn transmit(&self, frame: MockFrame) {
        let peers = self.peers.lock().await;
        for peer in peers.iter() {
            let _ = peer.send(frame.clone());
        }
    }
}

struct TokioTx {
    bus: Arc<TokioBus>,
}

struct TokioRx {
    rx: mpsc::UnboundedReceiver<MockFrame>,
}

impl embedded_can_interface::AsyncTxFrameIo for TokioTx {
    type Frame = MockFrame;
    type Error = TokioCanError;

    async fn send(&mut self, frame: &Self::Frame) -> Result<(), Self::Error> {
        self.bus.transmit(frame.clone()).await;
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

impl embedded_can_interface::AsyncRxFrameIo for TokioRx {
    type Frame = MockFrame;
    type Error = TokioCanError;

    async fn recv(&mut self) -> Result<Self::Frame, Self::Error> {
        self.rx.recv().await.ok_or(TokioCanError::Closed)
    }

    async fn recv_timeout(&mut self, _timeout: Duration) -> Result<Self::Frame, Self::Error> {
        self.recv().await
    }

    async fn wait_not_empty(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct TokioRt;

impl AsyncRuntime for TokioRt {
    type TimeoutError = tokio::time::error::Elapsed;

    type Sleep<'a> = tokio::time::Sleep
    where
        Self: 'a;

    fn sleep<'a>(&'a self, duration: Duration) -> Self::Sleep<'a> {
        tokio::time::sleep(duration)
    }

    type Timeout<'a, F> = tokio::time::Timeout<F>
    where
        Self: 'a,
        F: core::future::Future + 'a;

    fn timeout<'a, F>(&'a self, duration: Duration, future: F) -> Self::Timeout<'a, F>
    where
        F: core::future::Future + 'a,
    {
        tokio::time::timeout(duration, future)
    }
}

fn cfg_pair(a_to_b: u16, b_to_a: u16) -> (IsoTpConfig, IsoTpConfig) {
    (
        IsoTpConfig {
            tx_id: Id::Standard(embedded_can::StandardId::new(a_to_b).unwrap()),
            rx_id: Id::Standard(embedded_can::StandardId::new(b_to_a).unwrap()),
            ..IsoTpConfig::default()
        },
        IsoTpConfig {
            tx_id: Id::Standard(embedded_can::StandardId::new(b_to_a).unwrap()),
            rx_id: Id::Standard(embedded_can::StandardId::new(a_to_b).unwrap()),
            ..IsoTpConfig::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_single_frame_roundtrip() {
    let bus = Arc::new(TokioBus::new());
    let (tx_a, rx_a) = bus.add_interface().await;
    let (tx_b, rx_b) = bus.add_interface().await;

    let (cfg_a, cfg_b) = cfg_pair(0x123, 0x321);
    let mut node_a = IsoTpAsyncNode::with_std_clock(tx_a, rx_a, cfg_a).unwrap();
    let mut node_b = IsoTpAsyncNode::with_std_clock(tx_b, rx_b, cfg_b).unwrap();

    let rt = TokioRt;
    let payload = b"hello";

    let recv_fut = async {
        let mut delivered = Vec::new();
        node_b
            .recv(&rt, Duration::from_millis(200), &mut |data| delivered = data.to_vec())
            .await
            .unwrap();
        delivered
    };
    let send_fut = async { node_a.send(&rt, payload, Duration::from_millis(200)).await.unwrap() };

    let (delivered, ()) = tokio::join!(recv_fut, send_fut);
    assert_eq!(delivered, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_multi_frame_roundtrip() {
    let bus = Arc::new(TokioBus::new());
    let (tx_a, rx_a) = bus.add_interface().await;
    let (tx_b, rx_b) = bus.add_interface().await;

    let (cfg_a, cfg_b) = cfg_pair(0x700, 0x701);
    let mut node_a = IsoTpAsyncNode::with_std_clock(tx_a, rx_a, cfg_a).unwrap();
    let mut node_b = IsoTpAsyncNode::with_std_clock(tx_b, rx_b, cfg_b).unwrap();

    let rt = TokioRt;
    let payload: Vec<u8> = (0..100).collect();

    let recv_fut = async {
        let mut delivered = Vec::new();
        node_b
            .recv(&rt, Duration::from_millis(500), &mut |data| delivered = data.to_vec())
            .await
            .unwrap();
        delivered
    };
    let send_fut =
        async { node_a.send(&rt, &payload, Duration::from_millis(500)).await.unwrap() };

    let (delivered, ()) = tokio::join!(recv_fut, send_fut);
    assert_eq!(delivered, payload);
}

