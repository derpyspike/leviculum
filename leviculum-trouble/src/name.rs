//! The name this node advertises and answers with.
//!
//! The scan response is rebuilt at every advertising start
//! (`ble::columba`), so the value here is live: the operator's name when
//! the firmware set one, `LN-<hex8>` from the identity hash otherwise —
//! the same rule every family applies (`leviculum-node-name`).

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use leviculum_core::node_name::NodeName;

static IDENTITY: BlockingMutex<CriticalSectionRawMutex, Cell<Option<[u8; 16]>>> =
    BlockingMutex::new(Cell::new(None));
static CONFIGURED: BlockingMutex<CriticalSectionRawMutex, Cell<Option<NodeName>>> =
    BlockingMutex::new(Cell::new(None));

/// Hand the boot identity over, before the protocol tasks start; without
/// it the advertised name is empty rather than wrong.
pub fn note_boot_identity(identity_hash: [u8; 16]) {
    IDENTITY.lock(|cell| cell.set(Some(identity_hash)));
}

/// Set an operator name; `None` falls back to `LN-<hex8>`.
pub fn set_configured(name: Option<NodeName>) {
    CONFIGURED.lock(|cell| cell.set(name));
}

/// The BLE name in force right now.
pub fn gap_name_now() -> NodeName {
    let Some(identity_hash) = IDENTITY.lock(|cell| cell.get()) else {
        return NodeName::EMPTY;
    };
    let configured = CONFIGURED.lock(|cell| cell.get());
    leviculum_node_name::gap_name_for(&identity_hash, configured.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_name_is_ln_hex8_of_the_identity() {
        // The seam test in `ble::columba` sets a configured name; the
        // shared lock keeps it out of this one's window.
        let _ring = crate::log::test_ring::lock();
        note_boot_identity([0xab, 0xcd, 0x12, 0x34, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(gap_name_now().as_str(), "LN-abcd1234");
    }
}
