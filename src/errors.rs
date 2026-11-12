//! Transport-layer error types.

/// Timeout category identifiers (ISO-TP naming).
///
/// These names follow the ISO-TP timeout terminology:
/// - `N_As` / `N_Ar` are sender/receiver timeouts for CAN frame transmission/reception.
/// - `N_Bs` / `N_Br` relate to flow control and consecutive frame reception.
/// - `N_Cs` relates to pacing between consecutive frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    /// Timeout while sending a frame.
    NAs,
    /// Timeout while waiting for receive queue.
    NAr,
    /// Timeout waiting for flow control.
    NBs,
    /// Timeout waiting for consecutive frame.
    NBr,
    /// Timeout between consecutive frame sends.
    NCs,
}

/// Transport-layer errors surfaced by the ISO-TP implementation.
#[derive(Debug)]
#[non_exhaustive]
pub enum IsoTpError<E> {
    /// Deadline exceeded for the indicated phase.
    Timeout(TimeoutKind),
    /// Frame received in an unexpected state.
    UnexpectedPdu,
    /// Sequence number mismatch.
    BadSequence,
    /// Remote side indicated overflow or length invalid.
    Overflow,
    /// Malformed CAN frame content.
    InvalidFrame,
    /// Configuration rejected at construction time.
    InvalidConfig,
    /// Backend would block in non-blocking mode.
    WouldBlock,
    /// Receive buffer could not fit data.
    RxOverflow,
    /// Operation attempted while a transfer is active.
    NotIdle,
    /// Wrapper around backend-specific errors.
    LinkError(E),
}

impl<E> From<E> for IsoTpError<E> {
    /// Convert a backend-specific error into [`IsoTpError::LinkError`].
    fn from(err: E) -> Self {
        IsoTpError::LinkError(err)
    }
}

impl<E: core::fmt::Display> core::fmt::Display for IsoTpError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Timeout(kind) => write!(f, "ISO-TP {kind:?} timeout"),
            Self::LinkError(error) => write!(f, "CAN link error: {error}"),
            Self::UnexpectedPdu => f.write_str("unexpected ISO-TP PDU"),
            Self::BadSequence => f.write_str("ISO-TP sequence mismatch"),
            Self::Overflow => f.write_str("ISO-TP payload overflow"),
            Self::InvalidFrame => f.write_str("invalid CAN frame"),
            Self::InvalidConfig => f.write_str("invalid ISO-TP configuration"),
            Self::WouldBlock => f.write_str("ISO-TP would block"),
            Self::RxOverflow => f.write_str("ISO-TP receive overflow"),
            Self::NotIdle => f.write_str("ISO-TP transfer already active"),
        }
    }
}
impl<E: core::error::Error + 'static> core::error::Error for IsoTpError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::LinkError(error) => Some(error),
            _ => None,
        }
    }
}
