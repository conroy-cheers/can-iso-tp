# Changelog

This file lists the notable changes to `can-iso-tp`, starting with 0.3.0. The
crate is pre-1.0, so under semver a minor version bump is a breaking change.
The [thincan migration guide](https://github.com/conroy-cheers/thincan/blob/master/docs/migration-0.2.md)
covers the upgrade of the whole stack.

## 0.3.0 (2026-11-12)

### Breaking

- The crate now depends on `can-isotp-interface` 0.2.
- The `AsyncWithRt` adapters are removed. They implemented the removed
  `IsoTpAsyncEndpoint` and `IsoTpAsyncEndpointRecvInto` traits, and they do not
  meet the cancellation rules of the new traits. For async use through
  `can-isotp-interface`, run `driver::run_driver`.
- `IsoTpError` is `#[non_exhaustive]`. Matches on it outside this crate need a
  wildcard arm.
- The minimum supported Rust version is 1.88.

### Added

- The `async-driver` feature with `driver::run_driver`. The driver owns the
  ISO-TP protocol state in one persistent task. Applications send and receive
  through `can-isotp-queue` 0.3 ports, so a cancelled caller never leaves a
  transfer half done. The driver works without allocation.
- `DriverHooks`, `DriverStop` (`#[non_exhaustive]`) and the `DriverResources`,
  `DriverTx`, `DriverRx` and `DriverPorts` aliases.
- Demux methods that the driver uses: `tick`, `take_receive_timeout`,
  `next_wakeup`, `on_tx_frame_accepted` and `reset_send`.
- `IsoTpError` implements `Display` and `core::error::Error`.

### Unchanged

- The blocking and polling nodes, the mutable async node and demux APIs for
  serialized use, and the blocking `can-isotp-interface` implementations.
