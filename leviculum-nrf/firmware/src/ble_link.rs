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

#[cfg(test)]
mod tests {
    /// Audit 1715: the family's one install point puts the firmware's log
    /// under the shared crate. Before the call a `leviculum_ble_link` line
    /// goes nowhere (no sink); after it, the same line is in `LOG_RING`.
    #[test]
    fn channels_installs_the_firmware_log_as_the_shared_crates_sink() {
        crate::log::open_runtime_drain();
        let ring_text = || {
            let mut out = alloc::string::String::new();
            let mut chunk = [0u8; 256];
            loop {
                let n = crate::log::LOG_RING.read(&mut chunk);
                if n == 0 {
                    return out;
                }
                out.push_str(&alloc::string::String::from_utf8_lossy(&chunk[..n]));
            }
        };
        let _ = ring_text();
        let _channels = super::channels();
        leviculum_ble_link::session::note_dead_end(0x00c0_ffee_1715, "sink-proof");
        let text = ring_text();
        assert!(
            text.contains("[BLE ] BLE_DIAL_DEAD_END addr=00c0ffee1715 reason=sink-proof"),
            "the shared crate's line did not reach the firmware ring: {text:?}"
        );
    }
}
