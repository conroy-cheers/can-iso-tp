#![cfg(feature = "async-driver")]
use can_iso_tp::{Clock, IsoTpConfig, IsoTpError, RxStorage, driver::run_driver};
use can_isotp_interface::{Destination, IsoTpAsyncRead, IsoTpAsyncWrite, ReceiveError};
use can_isotp_queue::embassy::QueueResources;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_can::Frame;
use embedded_can_interface::{AsyncRxFrameIo, AsyncTxFrameIo};
use embedded_can_mock::MockFrame;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
#[derive(Clone, Copy)]
struct TokioClock;
impl Clock for TokioClock {
    type Instant = tokio::time::Instant;
    fn now(&self) -> Self::Instant {
        Self::Instant::now()
    }
    fn elapsed(&self, earlier: Self::Instant) -> Duration {
        earlier.elapsed()
    }
    fn add(&self, instant: Self::Instant, duration: Duration) -> Self::Instant {
        instant.checked_add(duration).unwrap()
    }
}
struct Tx(mpsc::Sender<MockFrame>);
struct Rx {
    frames: mpsc::Receiver<MockFrame>,
    calls: Arc<AtomicUsize>,
}
impl AsyncTxFrameIo for Tx {
    type Frame = MockFrame;
    type Error = &'static str;
    async fn send(&mut self, frame: &MockFrame) -> Result<(), Self::Error> {
        self.0.send(frame.clone()).await.map_err(|_| "CAN closed")
    }
    async fn send_timeout(&mut self, frame: &MockFrame, _: Duration) -> Result<(), Self::Error> {
        self.send(frame).await
    }
}
impl AsyncRxFrameIo for Rx {
    type Frame = MockFrame;
    type Error = &'static str;
    async fn recv(&mut self) -> Result<MockFrame, Self::Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.frames.recv().await.ok_or("CAN closed")
    }
    async fn recv_timeout(&mut self, _: Duration) -> Result<MockFrame, Self::Error> {
        panic!("driver must not poll timed receive")
    }
    async fn wait_not_empty(&mut self) -> Result<(), Self::Error> {
        panic!("driver uses receive readiness")
    }
}
type Resources = QueueResources<
    NoopRawMutex,
    IsoTpError<&'static str>,
    IsoTpError<&'static str>,
    tokio::time::Instant,
    256,
    2,
    2,
>;
fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(2)
}
fn cfg() -> IsoTpConfig {
    IsoTpConfig {
        max_payload_len: 256,
        frame_len: 8,
        block_size: 2,
        st_min: Duration::from_millis(1),
        ..Default::default()
    }
}
fn storages() -> [RxStorage<'static>; 2] {
    core::array::from_fn(|_| RxStorage::Owned(vec![0; 256]))
}
#[tokio::test]
async fn idle_wait_has_no_polling_and_full_duplex_segmentation_survives_cancelled_app_receive() {
    let ra = Resources::new();
    let rb = Resources::new();
    let (mut a_tx, mut a_rx, a_ports) = ra.split();
    let (mut b_tx, mut b_rx, b_ports) = rb.split();
    let (ab, a_frames) = mpsc::channel(8);
    let (ba, b_frames) = mpsc::channel(8);
    let a_calls = Arc::new(AtomicUsize::new(0));
    let b_calls = Arc::new(AtomicUsize::new(0));
    let da = run_driver(
        Tx(ab),
        Rx {
            frames: b_frames,
            calls: a_calls.clone(),
        },
        TokioClock,
        cfg(),
        0x11,
        None,
        storages(),
        a_ports,
        (),
        core::future::pending(),
    );
    let db = run_driver(
        Tx(ba),
        Rx {
            frames: a_frames,
            calls: b_calls.clone(),
        },
        TokioClock,
        cfg(),
        0x22,
        None,
        storages(),
        b_ports,
        (),
        core::future::pending(),
    );
    let app = async {
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(a_calls.load(Ordering::Relaxed), 1);
        assert_eq!(b_calls.load(Ordering::Relaxed), 1);
        let mut a_out = [0; 256];
        let mut b_out = [0; 256];
        assert!(
            tokio::time::timeout(Duration::from_millis(5), a_rx.recv_into(&mut a_out))
                .await
                .is_err()
        );
        a_tx.send(Destination::Physical(0x22), &[7; 127], deadline())
            .await
            .unwrap();
        b_tx.send(Destination::Physical(0x11), &[9; 193], deadline())
            .await
            .unwrap();
        let (a, b) = tokio::join!(a_rx.recv_into(&mut a_out), b_rx.recv_into(&mut b_out));
        assert_eq!(a.unwrap().meta.reply_to, 0x22);
        assert_eq!(b.unwrap().meta.reply_to, 0x11);
        assert_eq!(&a_out[..193], &[9; 193]);
        assert_eq!(&b_out[..127], &[7; 127]);
        ra.close();
        rb.close();
    };
    let (a, b, ()) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(da, db, app) })
            .await
            .unwrap();
    a.unwrap();
    b.unwrap();
}
#[tokio::test]
async fn accepted_tx_outlives_deadline_and_application_cancellation() {
    let resources = Resources::new();
    let (mut tx, _rx, ports) = resources.split();
    let (output, mut frames) = mpsc::channel(8);
    let (input, rx) = mpsc::channel(8);
    let calls = Arc::new(AtomicUsize::new(0));
    let driver = run_driver(
        Tx(output),
        Rx { frames: rx, calls },
        TokioClock,
        cfg(),
        0x11,
        None,
        storages(),
        ports,
        (),
        core::future::pending(),
    );
    let app = async {
        tx.send(
            Destination::Physical(0x22),
            &[7; 20],
            tokio::time::Instant::now() + Duration::from_millis(10),
        )
        .await
        .unwrap();
        assert_eq!(frames.recv().await.unwrap().data()[0] >> 4, 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let fc = MockFrame::new(
            embedded_can::ExtendedId::new(0x18DA1122).unwrap(),
            &[0x30, 0, 0],
        )
        .unwrap();
        input.send(fc).await.unwrap();
        assert_eq!(frames.recv().await.unwrap().data()[0], 0x21);
        assert_eq!(frames.recv().await.unwrap().data()[0], 0x22);
        resources.close();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(driver, app) })
            .await
            .unwrap();
    result.unwrap();
}
#[tokio::test]
async fn raw_link_failure_closes_application_ports_with_original_owner_cause() {
    let resources = Resources::new();
    let (mut tx, mut rx, ports) = resources.split();
    let (output, _) = mpsc::channel(8);
    let (input, frames) = mpsc::channel(8);
    drop(input);
    let driver = run_driver(
        Tx(output),
        Rx {
            frames,
            calls: Arc::new(AtomicUsize::new(0)),
        },
        TokioClock,
        cfg(),
        0x11,
        None,
        storages(),
        ports,
        (),
        core::future::pending(),
    );
    assert!(matches!(
        driver.await,
        Err(IsoTpError::LinkError("CAN closed"))
    ));
    assert!(matches!(
        tx.send(Destination::Physical(0x22), &[1], deadline()).await,
        Err(can_isotp_interface::TransmitError::Closed)
    ));
    assert!(matches!(
        rx.recv_into(&mut [0; 8]).await,
        Err(ReceiveError::Closed)
    ));
}

#[tokio::test]
async fn raw_send_future_is_retained_across_an_incoming_frame() {
    struct TrackedTx {
        sender: mpsc::Sender<MockFrame>,
        starts: Arc<AtomicUsize>,
    }
    impl AsyncTxFrameIo for TrackedTx {
        type Frame = MockFrame;
        type Error = &'static str;
        async fn send(&mut self, frame: &MockFrame) -> Result<(), Self::Error> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            self.sender.send(frame.clone()).await.map_err(|_| "closed")
        }
        async fn send_timeout(
            &mut self,
            frame: &MockFrame,
            _: Duration,
        ) -> Result<(), Self::Error> {
            self.send(frame).await
        }
    }
    let resources = Resources::new();
    let (mut tx, _rx, ports) = resources.split();
    let (output, mut frames) = mpsc::channel(1);
    let (input, rx) = mpsc::channel(2);
    output
        .send(MockFrame::new(embedded_can::ExtendedId::new(0x18DA2211).unwrap(), &[0]).unwrap())
        .await
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let driver = run_driver(
        TrackedTx {
            sender: output,
            starts: starts.clone(),
        },
        Rx {
            frames: rx,
            calls: calls.clone(),
        },
        TokioClock,
        cfg(),
        0x11,
        None,
        storages(),
        ports,
        (),
        core::future::pending(),
    );
    let app = async {
        tx.send(Destination::Physical(0x22), &[7], deadline())
            .await
            .unwrap();
        input
            .send(
                MockFrame::new(embedded_can::ExtendedId::new(0x18DA1133).unwrap(), &[1, 9])
                    .unwrap(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        frames.recv().await.unwrap();
        assert_eq!(frames.recv().await.unwrap().data()[1], 7);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        resources.close();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(driver, app) })
            .await
            .unwrap();
    result.unwrap();
}
#[tokio::test]
async fn receive_timeout_with_full_event_queue_stops_arming_the_expired_timer() {
    let resources = Resources::new();
    let (_tx, _rx, ports) = resources.split();
    let (output, mut outgoing) = mpsc::channel(8);
    let (input, frames) = mpsc::channel(8);
    let calls = Arc::new(AtomicUsize::new(0));
    let config = IsoTpConfig {
        n_br: Duration::from_millis(15),
        ..cfg()
    };
    let driver = run_driver(
        Tx(output),
        Rx {
            frames,
            calls: calls.clone(),
        },
        TokioClock,
        config,
        0x11,
        None,
        storages(),
        ports,
        (),
        core::future::pending(),
    );
    let app = async {
        let id = embedded_can::ExtendedId::new(0x18DA1122).unwrap();
        // Fill both RX events and the one retained driver event.
        input
            .send(MockFrame::new(id, &[1, 1]).unwrap())
            .await
            .unwrap();
        input
            .send(MockFrame::new(id, &[1, 2]).unwrap())
            .await
            .unwrap();
        input
            .send(
                MockFrame::new(
                    embedded_can::ExtendedId::new(0x18DA1133).unwrap(),
                    &[0x10, 20, 1, 2, 3, 4, 5, 6],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        outgoing.recv().await.unwrap();
        input
            .send(MockFrame::new(id, &[1, 3]).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let before = calls.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(calls.load(Ordering::Relaxed), before);
        resources.close();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(driver, app) })
            .await
            .unwrap();
    result.unwrap();
}
