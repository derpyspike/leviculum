//! Completed-packet handoff shared by both Columba connection roles.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use leviculum_ble_tx::{incoming_verdict, Enqueue, Fill};

/// Completed inbound packets refused by the bounded node channel.
pub static BLE_RX_QUEUE_FULL: AtomicU32 = AtomicU32::new(0);

/// Retire the completed packet once, whether the node accepts it or not.
/// Successful ownership transfer counts unframed receive bytes exactly once.
pub(crate) fn offer_incoming<const N: usize>(
    incoming_tx: &Sender<'_, CriticalSectionRawMutex, (Option<[u8; 16]>, Vec<u8>), N>,
    peer: Option<[u8; 16]>,
    packet: Vec<u8>,
    slot_index: usize,
) -> bool {
    let len = packet.len();
    let fill = Fill::new(incoming_tx.len(), incoming_tx.capacity());
    let queued =
        incoming_verdict(fill) == Enqueue::Queue && incoming_tx.try_send((peer, packet)).is_ok();
    if queued {
        crate::iface_bytes::note_rx(crate::iface_bytes::BLE, len);
    } else {
        let rx_full = BLE_RX_QUEUE_FULL.fetch_add(1, Ordering::Relaxed) + 1;
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_RX_QUEUE_FULL slot={} len={} depth={} rx_full={}",
                slot_index,
                len,
                incoming_tx.capacity(),
                rx_full,
            ),
        );
    }
    queued
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_sync::channel::Channel;

    #[test]
    fn saturation_refuses_only_the_completed_packet_and_counts_the_drop() {
        let _ring = crate::log::test_ring::lock();
        let queue: Channel<CriticalSectionRawMutex, (Option<[u8; 16]>, Vec<u8>), 2> =
            Channel::new();
        let sender = queue.sender();
        let peer = [0x31; 16];
        let before = BLE_RX_QUEUE_FULL.load(Ordering::Relaxed);
        let before_bytes = crate::iface_bytes::snapshot(crate::iface_bytes::BLE).rxb;
        assert!(offer_incoming(&sender, Some(peer), alloc::vec![1, 2], 0));
        assert!(offer_incoming(&sender, None, alloc::vec![3], 1));
        assert!(!offer_incoming(
            &sender,
            Some(peer),
            alloc::vec![4, 5, 6],
            0
        ));
        assert_eq!(BLE_RX_QUEUE_FULL.load(Ordering::Relaxed), before + 1);
        assert_eq!(
            crate::iface_bytes::snapshot(crate::iface_bytes::BLE).rxb,
            before_bytes + 3
        );
        assert_eq!(
            queue.try_receive().unwrap(),
            (Some(peer), alloc::vec![1, 2])
        );
        assert!(offer_incoming(&sender, Some(peer), alloc::vec![7], 0));
        assert_eq!(queue.try_receive().unwrap(), (None, alloc::vec![3]));
        assert_eq!(queue.try_receive().unwrap(), (Some(peer), alloc::vec![7]));
        assert!(queue.try_receive().is_err());
        assert_eq!(BLE_RX_QUEUE_FULL.load(Ordering::Relaxed), before + 1);
        assert_eq!(
            crate::iface_bytes::snapshot(crate::iface_bytes::BLE).rxb,
            before_bytes + 4
        );
    }
}
