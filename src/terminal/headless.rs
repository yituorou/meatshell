//! The transport helpers used by SSH automation, without terminal UI rendering.
//! Both build modes compile the same implementations.
#[path = "impls/encoding.rs"]
mod encoding;
#[path = "impls/zmodem.rs"]
pub(crate) mod zmodem;
pub(crate) use encoding::TerminalEncoding;
