//! The seam's host fake (test builds only), moved here with the seam
//! (1715): `leviculum-firmware`'s other seam fakes stay in its own
//! `fakes` module.

use alloc::vec::Vec;

use crate::link::LinkTable;
use crate::seam::{BleLink, BleLinkEvent};
use leviculum_ble_tx::{
    plan_fanout, resolve_fanout, BleRole, Delivery, DrainRouter, LinkUp, Origin, PeerRegistry,
    TxAim,
};
use leviculum_core::node_name::NodeName;
use leviculum_heap_budget::MAX_LINKS;

/// A BLE backend whose slot map is the real one: a
/// [`PeerRegistry`] for which identity holds which slot and the
/// duplicate rules, a [`DrainRouter`] for which slots a connection holds,
/// and [`BleLink::send`] resolved exactly as the SoftDevice backend's
/// fan-out resolves it (`plan_fanout`, then `resolve_fanout`).
///
/// Scripted from the test: a connection comes up ([`connect`](Self::connect)),
/// presents its identity ([`handshake`](Self::handshake), or both at once
/// with [`link_up`](Self::link_up)), carries a frame in
/// ([`frame_in`](Self::frame_in)), goes down ([`link_down`](Self::link_down)).
/// Each peer transition the backend would report is entered into
/// [`table`](Self::table) through the same [`LinkTable::note`]
/// `deliver` calls, and kept in [`events`](Self::events).
pub struct FakeBleLink {
    registry: PeerRegistry<MAX_LINKS>,
    drain: DrainRouter<MAX_LINKS>,
    /// The identity each slot's session learned at its handshake, as the
    /// backend's session task holds it: what an inbound frame names.
    identities: [Option<[u8; 16]>; MAX_LINKS],
    next_handle: u16,
    /// The node's link table, fed by the reports.
    pub table: LinkTable<MAX_LINKS>,
    /// Every report, in order.
    pub events: Vec<BleLinkEvent>,
    /// Every send: the aim as it crossed the seam, where the slot map
    /// placed it, and the packet's length.
    pub sent: Vec<(TxAim, Delivery, usize)>,
    /// The advertised name, once set.
    pub name: Option<NodeName>,
}

impl Default for FakeBleLink {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBleLink {
    /// No connections.
    pub fn new() -> Self {
        Self {
            registry: PeerRegistry::new(),
            drain: DrainRouter::new(),
            identities: [None; MAX_LINKS],
            next_handle: 0,
            table: LinkTable::new(),
            events: Vec::new(),
            sent: Vec::new(),
            name: None,
        }
    }

    /// A connection comes up at `now_ms` and claims a slot, its identity
    /// not presented yet. Answers the slot.
    pub fn connect(&mut self, now_ms: u64) -> usize {
        let handle = self.next_handle;
        self.next_handle += 1;
        let (slot, _) = self.drain.claim_indexed(handle).expect("a free slot");
        self.registry.conn_up(slot, u64::from(handle), now_ms);
        slot
    }

    /// The connection on `slot` presents `identity` at `now_ms`; we hold
    /// it in `role`. A first link reports the peer up; a refused
    /// duplicate is dropped, as the backend drops it.
    pub fn handshake(
        &mut self,
        slot: usize,
        identity: [u8; 16],
        role: BleRole,
        now_ms: u64,
    ) -> LinkUp {
        let origin = match role {
            BleRole::Peripheral => Origin::Incoming,
            BleRole::Central => Origin::Outgoing,
        };
        let up = self.registry.link_up(slot, identity, origin, 247, now_ms);
        match up {
            LinkUp::Refused { .. } => self.release(slot),
            LinkUp::Accepted { first } => {
                self.identities[slot] = Some(identity);
                if first {
                    self.report(
                        BleLinkEvent::PeerUp {
                            identity,
                            role: origin.our_role(),
                        },
                        now_ms,
                    );
                }
            }
            LinkUp::Displaced { .. } => self.identities[slot] = Some(identity),
        }
        up
    }

    /// [`connect`](Self::connect) and [`handshake`](Self::handshake) at
    /// once. Answers the slot.
    pub fn link_up(&mut self, identity: [u8; 16], role: BleRole, now_ms: u64) -> usize {
        let slot = self.connect(now_ms);
        self.handshake(slot, identity, role, now_ms);
        slot
    }

    /// One completed packet arrives on `slot` at `now_ms`: the inbound
    /// report the backend hands up, naming the link's identity.
    pub fn frame_in(&mut self, slot: usize, packet: Vec<u8>, now_ms: u64) -> BleLinkEvent {
        self.registry.note_data(slot, now_ms);
        BleLinkEvent::Inbound {
            link: self.identities[slot],
            packet,
        }
    }

    /// The link on `slot` goes down at `now_ms`: its registry entry
    /// first, then its drain slot, the backend's teardown order. The
    /// identity's last link reports the peer lost.
    pub fn link_down(&mut self, slot: usize, now_ms: u64) {
        if let Some((identity, origin)) = self.registry.link_down(slot) {
            self.report(
                BleLinkEvent::PeerLost {
                    identity,
                    role: origin.our_role(),
                },
                now_ms,
            );
        }
        self.release(slot);
    }

    /// Where the last send went.
    pub fn last_delivery(&self) -> Option<Delivery> {
        self.sent.last().map(|(_, delivery, _)| *delivery)
    }

    fn release(&mut self, slot: usize) {
        self.identities[slot] = None;
        self.registry.conn_down(slot);
        if let Some(handle) = self.drain.handle_at(slot) {
            self.drain.release(handle);
        }
    }

    fn report(&mut self, event: BleLinkEvent, now_ms: u64) {
        assert!(
            self.table.note(&event, now_ms),
            "the table holds every peer"
        );
        self.events.push(event);
    }
}

impl BleLink for FakeBleLink {
    fn send(&mut self, aim: TxAim, packet: Vec<u8>) {
        let delivery = resolve_fanout(&self.drain, plan_fanout(&self.registry, aim));
        self.sent.push((aim, delivery, packet.len()));
    }

    fn set_advertised_name(&mut self, name: NodeName) {
        self.name = Some(name);
    }
}
