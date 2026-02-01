# iso-tp

Lightweight ISO-TP (ISO 15765-2) transport for CAN.

This crate provides blocking/polling and async ISO-TP nodes built on top of the
`embedded-can-interface` traits, plus supporting types for addressing, PDUs, and Tx/Rx state.
