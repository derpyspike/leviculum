//! The BLE link's node side — re-exported from `leviculum-ble-link`
//! since 1715.
//!
//! The module's contents (the carrier gate, the pacing value, the drain
//! table, the census facts, the node-facing channels with `deliver` and
//! `run_fanout`, the node's link table and the BLE `Interface`
//! implementation; 998, 999, 1003) moved into their own executor-free
//! crate so the TrouBLE backend (`leviculum-trouble`) can stand behind
//! the same seam without pulling this stratum's embassy-executor
//! dependency. Everything keeps its path through this re-export; the
//! documentation lives at the items in `leviculum_ble_link`.
//!
//! Two names are this module's own:
//!
//! * [`HVN_DRAIN`] — the shared drain table under the name this family
//!   has always used, after the SoftDevice event it routes
//!   (`BLE_GATTS_EVT_HVN_TX_COMPLETE`); the shared crate calls it
//!   `TX_DRAIN`, since TrouBLE's drain edge is the completed-packets
//!   event, not an HVN.
//! * [`channels`] — the shared `channels()`, which here also installs
//!   this crate's log module as the shared crate's sink. Every nRF
//!   binary takes its channels exactly once at boot, before any BLE
//!   task runs, so this is the family's one install point and no BLE
//!   line can be lost to a missing sink.

pub use leviculum_ble_link::TX_DRAIN as HVN_DRAIN;
pub use leviculum_ble_link::*;

/// The binaries' handle on the channels, taken once at boot — and the
/// nRF family's install point for the shared crate's log sink (see the
/// module doc).
pub fn channels() -> BleChannels {
    crate::log::hook_ble_link();
    leviculum_ble_link::channels()
}
