//! The BLE link's node side, shared by every BLE backend (order 1715).
//!
//! Until 1715 this was `leviculum_firmware::ble_link` plus the BLE rows
//! of its `seams` module, and the TrouBLE backend carried a copy of the
//! lot because it could not link the orchestration stratum: the stratum
//! names embassy-executor, which the M9 image must not pull (its core-0
//! loop polls futures itself; esp-rtos schedules only the radio blobs'
//! tasks). The copy is what Lew's rule of 2026-10-23 forbids, so the
//! node side moved down into this crate, which names no executor and no
//! HAL: `leviculum-firmware` re-exports it unchanged for the nRF world,
//! `leviculum-trouble` links it directly.
//!
//! What lives here is exactly the seam's node half: the
//! [`BleLink`]/[`BleLinkEvent`] seam itself, the carrier gate, the
//! pacing value, the drain table, the node's link table, the node-facing
//! channels with [`deliver`] and [`run_fanout`], the census facts and
//! the BLE [`Interface`](leviculum_core::traits::Interface)
//! implementation. What stays in each backend is the slot map: which
//! connection, drain slot and fragmenter carries which identity, and the
//! duplicate rules that decide which of two links a peer keeps.
//!
//! Two families, two log sinks: the lines this crate writes go through
//! [`log::set_sink`], installed once at boot by whoever embeds it (the
//! nRF binaries through `leviculum_firmware::ble_link::channels`, the
//! M9 in its BLE bring-up). Before a sink is installed the lines are
//! dropped, which no boot path reaches: both families install the sink
//! before any BLE task runs.

#![no_std]

extern crate alloc;

pub mod log;
pub mod session;
pub mod traffic;

mod link;
mod seam;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

pub use link::*;
pub use seam::{BleLink, BleLinkEvent};
