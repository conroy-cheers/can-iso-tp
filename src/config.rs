//! ISO-TP configuration container.

use core::time::Duration;
use embedded_can_interface::Id;

/// Configuration for an ISO-TP node.
#[derive(Debug, Clone)]
pub struct IsoTpConfig {
    /// CAN identifier used when transmitting ISO-TP PDUs.
    pub tx_id: Id,
    /// CAN identifier expected when receiving ISO-TP PDUs.
    pub rx_id: Id,
    /// Number of consecutive frames allowed before requesting a new flow control (0 = unlimited).
    pub block_size: u8,
    /// Minimum separation time enforced between consecutive frames.
    pub st_min: Duration,
    /// Maximum number of FlowControl::Wait responses accepted before failing.
    pub wft_max: u8,
    /// Optional padding byte for transmitted frames (None = no padding).
    pub padding: Option<u8>,
    /// Maximum application payload length accepted.
    pub max_payload_len: usize,
    /// Length of the receive buffer.
    pub rx_buffer_len: usize,
    /// Timeout waiting for transmit availability.
    pub n_as: Duration,
    /// Timeout waiting for receive availability.
    pub n_ar: Duration,
    /// Timeout waiting for flow control after First Frame.
    pub n_bs: Duration,
    /// Timeout waiting for Consecutive Frame while receiving.
    pub n_br: Duration,
    /// Timeout between consecutive frame transmissions.
    pub n_cs: Duration,
}

impl Default for IsoTpConfig {
    /// Baseline config with zeroed IDs and 4 KB payload limits.
    fn default() -> Self {
        Self {
            tx_id: Id::Standard(embedded_can::StandardId::new(0).unwrap()),
            rx_id: Id::Standard(embedded_can::StandardId::new(0).unwrap()),
            block_size: 0,
            st_min: Duration::from_millis(0),
            wft_max: 0,
            padding: None,
            max_payload_len: 4095,
            rx_buffer_len: 4095,
            n_as: Duration::from_millis(1000),
            n_ar: Duration::from_millis(1000),
            n_bs: Duration::from_millis(1000),
            n_br: Duration::from_millis(1000),
            n_cs: Duration::from_millis(1000),
        }
    }
}

impl IsoTpConfig {
    /// Reject invalid limits or mirrored IDs.
    pub fn validate(&self) -> Result<(), ()> {
        if self.max_payload_len == 0 || self.max_payload_len > 4095 {
            return Err(());
        }
        if self.rx_buffer_len < self.max_payload_len {
            return Err(());
        }
        if self.tx_id == self.rx_id {
            return Err(());
        }
        Ok(())
    }
}
