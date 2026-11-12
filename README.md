# can-iso-tp

Lightweight ISO-TP (ISO 15765-2) transport for CAN.

This crate provides blocking/polling and async ISO-TP nodes built on top of the
`embedded-can-interface` traits, plus supporting types for addressing, PDUs, and Tx/Rx state.

## no_std / no-alloc

Receive-side reassembly requires a buffer. In `no_std`/`no alloc` environments, you typically
provide a fixed slice (`&mut [u8]`). When `alloc` is available, you can also pass
`RxStorage::Owned(Vec<u8>)`.

## Independent async driver (0.3)

Enable `async-driver` for `driver::run_driver`. Split a fixed-storage
`can_isotp_queue::embassy::QueueResources` into application TX/RX and driver
ports, then run the driver independently of application futures. Supply a clock
whose instant implements `can_isotp_interface::Deadline`, per-peer RX buffers,
CAN frame halves, addressing, and shutdown. It works without allocation when
buffers and resources are borrowed.

Application sends report acceptance into owned protocol state. Accepted payload
leases persist until segmentation completes even if the caller or its deadline
ends. `DriverHooks::transmitted` receives the owned later completion result;
a supervisor can retain failures without attaching them to another command.
Raw link failures terminate the driver and close all ports. Protocol failures
are completion events and do not automatically replay commands.

Raw output is bounded by N_As; its pinned future survives incoming raw events.
One incoming raw frame can be retained while output is blocked; protocol
processing resumes after acceptance. TX pacing/flow-control timers then start
from actual frame acceptance. RX timer expiry remains independent of application
backpressure. Idle state arms no polling timer. The raw receiver must preserve
frames when its pending receive future is cancelled.

The old mutable async node/demux inherent APIs remain usable for explicitly
serialized applications. Their combined interface adapters and `AsyncWithRt`
were removed; they do not implement the new cancellation contract. Blocking
interface implementations remain available.
