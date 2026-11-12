//! Completed-packet handoff shared by both Columba connection roles.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use leviculum_ble_link::BleLinkEvent;

/// Completed inbound packets refused by the bounded node channel.
pub static BLE_RX_QUEUE_FULL: AtomicU32 = AtomicU32::new(0);

/// Retire the completed packet once, whether the node accepts it or not:
/// [`leviculum_ble_link::deliver`] owns the verdict, the census custody
/// and the receive byte count; this wrapper owns the backend's `rx_full`
/// counter and its line.
pub(crate) fn offer_incoming(peer: Option<[u8; 16]>, packet: Vec<u8>, slot_index: usize) -> bool {
    let len = packet.len();
    let queued = leviculum_ble_link::deliver(BleLinkEvent::Inbound { link: peer, packet });
    if !queued {
        let rx_full = BLE_RX_QUEUE_FULL.fetch_add(1, Ordering::Relaxed) + 1;
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_RX_QUEUE_FULL slot={} len={} rx_full={}",
                slot_index, len, rx_full,
            ),
        );
    }
    queued
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saturating the shared node channel refuses only the completed
    /// packet, counts the drop, and counts received bytes exactly once
    /// per accepted handoff — against the real shared channel, drained
    /// before and after (the scripted-controller test in `h4_tests` is
    /// the other consumer of the shared incoming channel in this binary;
    /// both hold the ring lock, so they never run at once).
    #[test]
    fn saturation_refuses_only_the_completed_packet_and_counts_the_drop() {
        let _ring = crate::log::test_ring::lock();
        let chans = leviculum_ble_link::channels();
        while chans.incoming_rx.try_receive().is_ok() {}
        let depth = leviculum_heap_budget::QUEUE_DEPTH;
        let peer = [0x31; 16];
        let before = BLE_RX_QUEUE_FULL.load(Ordering::Relaxed);
        let before_bytes = leviculum_ble_link::traffic::snapshot().rxb;
        for i in 0..depth {
            assert!(offer_incoming(Some(peer), alloc::vec![i as u8; 2], 0));
        }
        assert!(!offer_incoming(Some(peer), alloc::vec![9, 9, 9], 1));
        assert_eq!(BLE_RX_QUEUE_FULL.load(Ordering::Relaxed), before + 1);
        assert_eq!(
            leviculum_ble_link::traffic::snapshot().rxb,
            before_bytes + 2 * depth as u64
        );
        match chans.incoming_rx.try_receive() {
            Ok(BleLinkEvent::Inbound { link, packet }) => {
                assert_eq!(link, Some(peer));
                assert_eq!(packet, alloc::vec![0, 0]);
            }
            other => panic!("the first accepted packet, got {:?}", other),
        }
        assert!(offer_incoming(Some(peer), alloc::vec![7], 0));
        assert_eq!(BLE_RX_QUEUE_FULL.load(Ordering::Relaxed), before + 1);
        assert_eq!(
            leviculum_ble_link::traffic::snapshot().rxb,
            before_bytes + 2 * depth as u64 + 1
        );
        while let Ok(event) = chans.incoming_rx.try_receive() {
            if let BleLinkEvent::Inbound { packet, .. } = event {
                leviculum_ble_link::incoming_held_sub(packet.capacity());
            }
        }
    }
}
