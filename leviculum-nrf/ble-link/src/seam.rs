//! The BLE seam itself: the trait a backend implements and the events
//! it reports. One row of the seam table in
//! `docs/src/concepts/firmware-layers.md`, "The seams, and their shape";
//! `leviculum-firmware` re-exports both under `seams::` so the table's
//! paths stay true on the nRF side.

/// What a BLE backend reports — the genuine message half of the seam
/// (998's measurement; the census facts it also publishes are values of
/// this crate — [`crate::session_census`] and friends — not messages).
/// The backend constructs
/// these and hands them to [`crate::deliver`], which routes
/// each variant onto the channel the node loop reads it from: the two
/// peer transitions ride one ordered channel (a flap's `Lost` must land
/// before its `Up`'s pull decision), the inbound packets their own. The
/// peer transitions also feed the node's link table
/// ([`crate::LinkTable`], 1003) at that one entry, before they
/// are queued.
#[derive(Debug)]
pub enum BleLinkEvent {
    /// The identity's FIRST link on this interface is up. The main loop
    /// hands it to `NodeCore::handle_interface_peer_up`, which pulls the
    /// peer's delivery path over the new link — Columba answers a path
    /// request but announces on neither connect nor reconnect, so
    /// without the pull a relay that lost its direct entry stays blind
    /// until the peer's periodic announce.
    PeerUp {
        /// The 16 bytes the Columba handshake exchanged — the same
        /// value inbound packets name as their ingress link.
        identity: [u8; 16],
        /// OUR role on the link that brought the peer up
        /// ([`leviculum_ble_tx::Origin::our_role`]).
        role: leviculum_ble_tx::BleRole,
    },
    /// The identity's LAST link on this interface died. The main loop
    /// hands it to `NodeCore::handle_interface_peer_lost`, which culls
    /// the path entries whose next hop is that peer — without it a
    /// stale 1-hop BLE path keeps swallowing traffic after the peer
    /// walked out of range.
    PeerLost {
        /// The identity whose last link died.
        identity: [u8; 16],
        /// OUR role on the link that died.
        role: leviculum_ble_tx::BleRole,
    },
    /// One completed inbound Reticulum packet.
    Inbound {
        /// The identity the ingress link's peer presented in the
        /// handshake, or `None` for bytes that arrived before the
        /// handshake completed (Codeberg #365): path entries learned
        /// over the link stay attributable when it dies, even when the
        /// announce identity differs from the link identity (Columba).
        link: Option<[u8; 16]>,
        /// The reassembled packet.
        packet: alloc::vec::Vec<u8>,
    },
}

/// BLE link, the seam above GATT: the Columba link as the orchestration
/// sees it. Downward, this trait; upward, [`BleLinkEvent`] through
/// [`crate::deliver`].
///
/// The node side is this crate (998, 999, 1003, cut out of
/// `leviculum_firmware` by 1715): the carrier
/// gate, the pacing value, the drain table and the census facts the
/// backend publishes, which per-fragment work reads by direct call (the
/// per-packet decision of 998); the node-facing channels the events ride
/// and the `Interface` implementation; and since 1003 the node's link
/// table ([`crate::LinkTable`]), fed by the `PeerUp`/`PeerLost`
/// events. What stays below is the slot map: which connection, drain
/// slot and fragmenter carries which identity, and the duplicate rules
/// (#360, #412) that decide which of two links a peer keeps.
///
/// Nothing on this trait is per fragment. Carrier on/off is not on it
/// either: it is a pushed value ([`crate::set_active`], 998)
/// the backend's tasks wait on, and so is the pacing gap.
///
/// Today: the S140 SoftDevice backend in `leviculum-nrf`'s `ble/`
/// (`SoftDeviceLink`), and the TrouBLE backend in `leviculum-trouble`
/// (`TroubleLink`, 1715) over `bt_hci::Transport` on the ESP32-S3
/// radio — its `HciTransport` stays inside its backend, because the
/// SoftDevice is not an HCI controller. A BT5
/// broadcast backend (extended advertising, Coded PHY) has the same
/// shape: it resolves
/// [`TxAim::Flood`](leviculum_ble_tx::TxAim::Flood) over its
/// own carrier and reports peers through the same events.
pub trait BleLink {
    /// Place one outbound packet on the links `aim` names. The aim
    /// crosses the seam as the core stated it, and the backend resolves
    /// it over its own slot map: `Flood` reaches every link it holds,
    /// one that has not presented its identity yet included (the
    /// reviewer's decision on 999: an identity list could not name such
    /// a link, and today's fan-out serves it); `Peer` and `FloodExcept`
    /// name identities the backend maps to its links. A packet the aim
    /// cannot place is the backend's to drop and to say so.
    fn send(&mut self, aim: leviculum_ble_tx::TxAim, packet: alloc::vec::Vec<u8>);

    /// The name the backend advertises and answers with. Called once at
    /// bring-up, before the carrier first reads on; a backend that
    /// builds an advertisement from the name builds it from this value.
    /// A runtime rename is not part of the seam: the nRF's name module
    /// applies a new name at the next boot, so both BLE surfaces always
    /// show one name; the TrouBLE backend rebuilds its advertisement at
    /// every advertising start from the same pushed value.
    fn set_advertised_name(&mut self, name: leviculum_core::node_name::NodeName);
}
