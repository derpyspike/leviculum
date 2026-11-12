//! The GATT-independent session state both Columba backends share
//! (order 1715, condition 2): the live-peer registry and its lock, the
//! displacement machinery, the link expiry, the scan-phase clock, the
//! dead-end table, the dial backoff and the two ledgers, with their
//! `BLE_*` log lines.
//!
//! Until 1715 all of this stood twice, once in
//! `leviculum-nrf/src/ble/columba.rs` and once in
//! `leviculum-trouble/src/ble/columba.rs`, byte-for-byte for most of it.
//! What stays in each backend is everything that touches a GATT type or
//! the backend's own queues: the per-slot queue MOVE of a displacement
//! crosses back through the `move_queued` callback of
//! [`peer_link_up`], because the two families size their per-link
//! queues differently and the census is published by the family that
//! drains it.
//!
//! The decisions themselves are `leviculum-ble-tx`'s, host-tested; this
//! module is the board-global instance of each with its lock and its
//! line, exactly as the per-family copies were.

use core::cell::{Cell, RefCell};
use core::sync::atomic::AtomicU32;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{
    admit, Admission, AdvReport, ChurnLedger, ChurnPolicy, DeadEnds, DialBackoff, DialFailure,
    DialLedger, DialRefusedLine, DupRule, Ledgers, LedgerPolicy, LinkUp, LinkVerdict, Local,
    Origin, PeerAddr, PeerRegistry, PhaseEvent, PhaseNeighbours, ScanMode, ScanPhase, TxGap,
    DEAD_END_SLOTS, DEAD_END_TTL_MS, DIAL_BACKOFF_SLOTS, IDENTITY_HINT_LEN,
    PHASE_NEIGHBOUR_SLOTS, PERIPH_SLOTS,
};
use leviculum_core::framing::ble::{BleDefragmenter, DefragResult};
use leviculum_heap_budget::MAX_LINKS;

use crate::link::TX_DRAIN;
use crate::seam::BleLinkEvent;

/// The 16-byte identities of currently linked peers, indexed by the
/// link's drain-slot (the same index that selects its outbound queue).
///
/// v2.2 keys everything durable by identity precisely because BLE
/// addresses rotate (§"Why Not Use MAC Addresses as Keys?"). The
/// scanner cannot apply that lesson pre-connect — an advertisement
/// carries no identity — so a peer we already hold a link to can rotate
/// its address and reappear as a seemingly new device. This registry is
/// the post-connect closure of that hole: when an identity we already
/// hold connects again — read from the Identity characteristic on the
/// central path, presented in the handshake on the peripheral one — the
/// registry decides which of the two connections the pair keeps
/// ([`leviculum_ble_tx::judge_duplicate`], #360 round 2).
///
/// Since #360 round 2 the decision is the one BOTH sides compute: an
/// old link silent past [`leviculum_ble_tx::LINK_ABANDONED_MS`] — any
/// frame, keepalives included — was abandoned by its peer and loses to
/// the newcomer (`rule=abandoned`); an old link alive is arbitrated by
/// Columba's own `preferredBleRole`
/// ([`leviculum_ble_tx::preferred_ble_role`]), evaluated from the
/// PEER's perspective, and we keep the connection the peer keeps
/// (`rule=columba_mtu` / `columba_identity`). The 2026-09-12 21:41
/// field T114 is why round 1's payload-recency rule had to go: the
/// phone held a LIVE central link (keepalive 7.3 s old), the board
/// displaced it because its last payload was 15 185 ms old, the
/// phone's own arbitration kept it (fresh connection still at the MTU
/// floor in its ledger), and each side then closed the link the other
/// had kept — 45 s of dead air per cycle, from the opposite direction
/// of the morning failure round 1 fixed. The losing link is
/// disconnected BY US at the decision, whichever role, never left to
/// the expiry. A dead link is still cleared by the expiry
/// ([`link_silent`] at [`leviculum_ble_tx::LINK_TIMEOUT_MS`], the
/// sweep lnsd runs over its rows), which removes it whether or not
/// anything dials it. Every line carries `origin=` (the role map the
/// arbitration is computed over), `rule=`, both usable MTUs as
/// compared, the old link's `old_silence_ms` (the abandonment test's
/// input) and `old_data_silence_ms` (round 1's input, reported for
/// comparison, no longer consulted).
///
/// The first/last-link rules live host-tested in
/// [`leviculum_ble_tx::registry`]; this is the one firmware instance,
/// behind the critical-section mutex the host crate cannot need.
///
/// A blocking mutex over a `RefCell`: written from both roles' session
/// tasks and read from the central task's scan callback.
static LIVE_PEERS: BlockingMutex<CriticalSectionRawMutex, RefCell<PeerRegistry<MAX_LINKS>>> =
    BlockingMutex::new(RefCell::new(PeerRegistry::new()));

/// Links displaced by a newer connection of the same identity (#376,
/// #382), for the `BLE_COUNTERS` line's `displaced=` field.
pub static BLE_LINKS_DISPLACED: AtomicU32 = AtomicU32::new(0);

/// Connections refused because the identity behind them already holds a
/// LIVE link (#376), for the `BLE_COUNTERS` line's `refused=` field.
/// On a board sitting beside a Columba phone this is the counter that
/// should climb while `displaced=` stays put.
pub static BLE_LINKS_REFUSED: AtomicU32 = AtomicU32::new(0);

/// Payload writes refused because they arrived before the 16-byte
/// identity (#548), for the `BLE_COUNTERS` line's `unidentified=`
/// field. Each also names itself as a `BLE_PRE_HANDSHAKE_REFUSE` line,
/// written by the backend that refused it.
pub static BLE_PRE_HANDSHAKE_REFUSED: AtomicU32 = AtomicU32::new(0);

/// Peripheral link slots currently spent on a session, for the
/// advertised free-slot count (#375 item 3). Not a gate.
static PERIPH_BUSY: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// One claimed peripheral slot; dropping it frees the count.
pub struct PeripheralSlot;

impl PeripheralSlot {
    pub fn claim() -> Self {
        PERIPH_BUSY.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Self
    }
}

impl Drop for PeripheralSlot {
    fn drop(&mut self) {
        PERIPH_BUSY.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Peripheral slots still free, as the capability record advertises it.
pub fn free_peripheral_slots() -> u8 {
    PERIPH_SLOTS.saturating_sub(PERIPH_BUSY.load(core::sync::atomic::Ordering::Relaxed))
}

/// Hand the registry this node's own identity, before the protocol
/// tasks start (the self-link refusal reads it).
pub fn set_local_identity(identity_hash: [u8; 16]) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().set_local_identity(identity_hash));
}

/// Per-slot displacement latches (#376): [`peer_link_up`] signals the
/// OLD link's slot when a newer connection of the same identity takes
/// over, and the session holding that slot tears itself down through
/// its [`link_over`] arm. The payload is the connection handle the
/// displacer read out of the drain table, so a wake aimed at a previous
/// tenancy of the slot can never kill an innocent successor.
static DISPLACE_WAKES: [Signal<CriticalSectionRawMutex, u16>; MAX_LINKS] =
    [const { Signal::new() }; MAX_LINKS];

/// Resolve when this session's link has been displaced by a newer
/// connection of the same identity (#376). A latched wake for a
/// different handle is a stale leftover from a previous tenancy:
/// consumed and ignored.
async fn displaced(slot_index: usize, conn_handle: u16) {
    let wake = &DISPLACE_WAKES[slot_index];
    loop {
        if wake.wait().await == conn_handle {
            return;
        }
    }
}

/// Resolve when this session's link has delivered NOTHING — no packet
/// fragment, no keepalive — for [`leviculum_ble_tx::LINK_TIMEOUT_MS`]
/// (the board's link expiry, #382), or when its connection has not
/// handshaked within [`leviculum_ble_tx::HANDSHAKE_TIMEOUT_MS`] (#548),
/// returning which and the evidence. It sleeps exactly to the earliest
/// instant the link could be over; the decision is
/// [`PeerRegistry::link_verdict`], host-tested in ble-tx.
async fn link_silent(slot_index: usize) -> LinkEnd {
    loop {
        let now_ms = Instant::now().as_millis();
        match LIVE_PEERS.lock(|peers| peers.borrow().link_verdict(slot_index, now_ms)) {
            LinkVerdict::Wait(ms) => Timer::after_millis(ms).await,
            LinkVerdict::Expired(silence_ms) => return LinkEnd::Expired(silence_ms),
            LinkVerdict::HandshakeOverdue(waited_ms) => {
                return LinkEnd::HandshakeOverdue(waited_ms)
            }
        }
    }
}

/// Why the fourth arm of both session selects ended the link: the peer
/// built a replacement, or the link stopped delivering anything at all.
/// Both end in a disconnect; they differ in what the capture is owed.
pub enum LinkEnd {
    /// A newer connection of this identity took the peer over (#376);
    /// the displacer has already logged the decision.
    Displaced,
    /// Nothing was heard on this link for
    /// [`leviculum_ble_tx::LINK_TIMEOUT_MS`] — the silence, in ms.
    Expired(u64),
    /// The connection never wrote its 16-byte identity within
    /// [`leviculum_ble_tx::HANDSHAKE_TIMEOUT_MS`] (#548) — how long it
    /// waited. No identity, so no peer to report lost.
    HandshakeOverdue(u64),
}

/// One line for an expiry, none for a displacement — that one is
/// already on the wire as `BLE_LINK_REPLACED`, logged by the displacer
/// with the evidence it decided on.
pub fn log_link_end(role: &str, slot_index: usize, conn_handle: u16, end: &LinkEnd) {
    match end {
        LinkEnd::Expired(silence_ms) => crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_LINK_EXPIRE role={} slot={} conn={} silence_ms={}",
                role, slot_index, conn_handle, silence_ms
            ),
        ),
        LinkEnd::HandshakeOverdue(waited_ms) => crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_HANDSHAKE_TIMEOUT role={} slot={} conn={} waited_ms={}",
                role, slot_index, conn_handle, waited_ms
            ),
        ),
        LinkEnd::Displaced => {}
    }
}

/// The fourth arm of both session selects: the two ways a link ends
/// without either side's carrier going away.
pub async fn link_over(slot_index: usize, conn_handle: u16) -> LinkEnd {
    match select(displaced(slot_index, conn_handle), link_silent(slot_index)).await {
        Either::First(()) => LinkEnd::Displaced,
        Either::Second(end) => end,
    }
}

/// Register a slot's peer and, when this is the identity's FIRST link,
/// report the arrival through [`crate::deliver`] so the transport
/// learns about the peer behind the new link (Codeberg #365) — the
/// mirror of [`peer_link_down`]'s last-link rule.
///
/// A second link from an identity we already hold is decided so that
/// both sides keep the same connection (#360 round 2): `false` means
/// REFUSED — the existing link is alive and the peer's arbitration
/// keeps it, and the caller must drop the connection it just made,
/// address included (`note_dead_end`). `true` means the link is
/// registered, either plainly or after replacing the old one. Either
/// way the decision is logged — `BLE_LINK_DUP … action=refuse` or
/// `BLE_LINK_REPLACED` — with the evidence the rule decided on.
///
/// A displacement registers the new link BEFORE the old one is
/// signalled, which is what makes the old teardown's `link_down` a
/// churn edge rather than a `Lost`/`Up` flap; `move_queued(old, new)`
/// is the backend's: it moves the old slot's queued packets onto the
/// new link's queue (the peer no longer reads the old connection) and
/// answers `(moved, dropped)` for the line. It runs synchronously
/// between the registry check and the wake, so the old session cannot
/// vanish in between.
pub fn peer_link_up(
    slot_index: usize,
    conn_handle: u16,
    peer: PeerAddr,
    peer_id: [u8; 16],
    origin: Origin,
    att_mtu: u16,
    move_queued: impl FnOnce(usize, usize) -> (usize, usize),
) -> bool {
    let peer_value = peer.value;
    let now_ms = Instant::now().as_millis();
    let up = LIVE_PEERS.lock(|peers| {
        peers
            .borrow_mut()
            .link_up(slot_index, peer_id, origin, att_mtu, now_ms)
    });
    match up {
        LinkUp::Refused {
            old_slot,
            rule,
            old_silence_ms,
            old_data_silence_ms,
            old_usable_mtu,
            new_usable_mtu,
        } => {
            BLE_LINKS_REFUSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            // A refusal brought no link up; the reset rule leaves the
            // strict clock alone for it (`PhaseEvent::resets_strict`).
            note_phase_event(PhaseEvent::LinkRefused);
            let old_conn = TX_DRAIN
                .handle_at(old_slot)
                .unwrap_or(leviculum_ble_tx::NO_CONN_HANDLE);
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    "BLE_LINK_DUP peer={:02x}{:02x}{:02x}{:02x} addr={:012x} action=refuse rule={} origin={} old_conn={} new_conn={} old_mtu={} new_mtu={} old_silence_ms={} old_data_silence_ms={}",
                    peer_id[0], peer_id[1], peer_id[2], peer_id[3],
                    peer_value, rule.as_str(), origin_str(origin), old_conn, conn_handle,
                    old_usable_mtu, new_usable_mtu, old_silence_ms,
                    leviculum_ble_tx::DataSilence(old_data_silence_ms),
                ),
            );
            return false;
        }
        LinkUp::Displaced {
            old_slot,
            rule,
            old_silence_ms,
            old_data_silence_ms,
            old_usable_mtu,
            new_usable_mtu,
        } => {
            displace_old_link(
                DisplacedOld {
                    old_slot,
                    new_slot: slot_index,
                    new_conn: conn_handle,
                    peer,
                    peer_id,
                    origin,
                    rule,
                    old_silence_ms,
                    old_data_silence_ms,
                    old_usable_mtu,
                    new_usable_mtu,
                },
                move_queued,
            );
            // A displacement is a peer we hold a link to, reconnecting:
            // never a new neighbour, so the fallback clock runs on
            // (#504). The identity is recorded all the same, so its
            // next plain link in this phase is known.
            let _ = meet_link(&peer_id);
            note_phase_event(PhaseEvent::LinkDisplaced);
        }
        LinkUp::Accepted { first } => {
            if first {
                let _ = crate::link::deliver(BleLinkEvent::PeerUp {
                    identity: peer_id,
                    role: origin.our_role(),
                });
            }
            // A NEW neighbour's link in either role restarts the
            // fallback clock (#375, narrowed by #504): the strict rule
            // gets its full bound to place the newcomer before the
            // scanner may dial against the sort.
            note_phase_event(PhaseEvent::LinkAccepted(meet_link(&peer_id)));
        }
    }
    true
}

/// A slot's peer delivered a KEEPALIVE — the liveness clock the expiry
/// reads (#382) and, since #360 round 2, the duplicate rule's
/// abandonment test.
pub fn note_heard(slot_index: usize) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().note_heard(slot_index, now_ms));
}

/// A slot's peer delivered real PAYLOAD — a fragment frame — which
/// feeds the expiry's liveness clock AND the payload clock every
/// duplicate decision REPORTS. Called per inbound FRAME, before
/// reassembly.
pub fn note_data(slot_index: usize) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().note_data(slot_index, now_ms));
}

/// The stable token `BLE_LINK_DUP origin=` carries, matching lnsd's.
fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::Incoming => "incoming",
        Origin::Outgoing => "outgoing",
    }
}

/// Everything [`displace_old_link`] needs, one struct rather than nine
/// positional arguments, so a caller cannot silently swap
/// `old_usable_mtu` for `new_usable_mtu` — the two numbers whose ORDER
/// is the whole arbitration (#360 round 2).
struct DisplacedOld {
    old_slot: usize,
    new_slot: usize,
    new_conn: u16,
    peer: PeerAddr,
    peer_id: [u8; 16],
    origin: Origin,
    rule: DupRule,
    old_silence_ms: u64,
    old_data_silence_ms: Option<u64>,
    old_usable_mtu: u16,
    new_usable_mtu: u16,
}

/// Tear down the OLD link of an identity whose newer connection just
/// registered (#376, replacement rule #360 round 2). The teardown is
/// OURS and immediate — the peer's own cancel does not drop the ACL
/// (field 2026-09-12 21:41:48.146) — and the old slot's queued packets
/// move to the new link's queue first, through the backend's
/// `move_queued`.
fn displace_old_link(
    displaced: DisplacedOld,
    move_queued: impl FnOnce(usize, usize) -> (usize, usize),
) {
    let DisplacedOld {
        old_slot,
        new_slot,
        new_conn,
        peer,
        peer_id,
        origin,
        rule,
        old_silence_ms,
        old_data_silence_ms,
        old_usable_mtu,
        new_usable_mtu,
    } = displaced;
    let peer_value = peer.value;
    let old_conn = TX_DRAIN
        .handle_at(old_slot)
        .unwrap_or(leviculum_ble_tx::NO_CONN_HANDLE);
    let (moved, dropped) = move_queued(old_slot, new_slot);
    BLE_LINKS_DISPLACED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    // A replacement is a departure too (#417): the identity left the
    // address the old link was made on, even though it came straight
    // back. Fed here, by the displacer, so both roles' displacements
    // are one site and the displaced session's own teardown stays
    // silent about it.
    note_peer_leave(&peer_id, peer, "replaced");
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_LINK_REPLACED peer={:02x}{:02x}{:02x}{:02x} addr={:012x} rule={} origin={} old_conn={} new_conn={} old_mtu={} new_mtu={} old_silence_ms={} old_data_silence_ms={} moved={} dropped={}",
            peer_id[0], peer_id[1], peer_id[2], peer_id[3],
            peer_value, rule.as_str(), origin_str(origin), old_conn, new_conn,
            old_usable_mtu, new_usable_mtu, old_silence_ms,
            leviculum_ble_tx::DataSilence(old_data_silence_ms), moved, dropped,
        ),
    );
    DISPLACE_WAKES[old_slot].signal(old_conn);
}

/// Clear a slot's registry entry and, when that took the peer's LAST
/// link, report the loss through [`crate::deliver`] so the transport
/// culls the paths via that peer (Codeberg #365). A same-identity link
/// on another slot (the displacement hand-over window) means the peer
/// is still reachable: registry churn, not a loss, so no report.
pub fn peer_link_down(slot_index: usize) {
    let lost = LIVE_PEERS.lock(|peers| peers.borrow_mut().link_down(slot_index));
    if let Some((identity, origin)) = lost {
        let _ = crate::link::deliver(BleLinkEvent::PeerLost {
            identity,
            role: origin.our_role(),
        });
    }
}

/// Register a connection's address at the connection event, in either
/// role — before any identity is known — and restart the strict phase
/// when the address is a NEW fixed one (#375 §0, #504).
pub fn conn_link_up(slot_index: usize, addr: PeerAddr) {
    let now_ms = Instant::now().as_millis();
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_up(slot_index, addr.value, now_ms));
    let neighbour = PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().meet_conn(addr));
    note_phase_event(PhaseEvent::ConnUp(neighbour));
}

/// Clear a connection's address at teardown. A departure is not a new
/// neighbour, so the strict phase runs on (#504).
pub fn conn_link_down(slot_index: usize) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_down(slot_index));
    note_phase_event(PhaseEvent::ConnDown);
}

/// Record that `identity` linked in the current phase and say whether
/// it is new to it ([`PhaseNeighbours::meet_link`]).
fn meet_link(identity: &[u8; 16]) -> leviculum_ble_tx::Neighbour {
    PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().meet_link(identity))
}

/// The number of distinct live peer identities the slot map holds —
/// the backend's own count beside the node's [`crate::LinkTable`].
pub fn live_peer_count() -> usize {
    LIVE_PEERS.lock(|peers| peers.borrow().peer_count())
}

/// Map the core's link statement onto link slots — the fan-out's one
/// question to the slot map, asked by each backend's `BleLink::send`.
/// The rules and their peer-gone decisions are host-tested in
/// [`leviculum_ble_tx::plan_fanout`]; this is the lock around the
/// board-global registry instance.
pub fn plan_fanout(aim: leviculum_ble_tx::TxAim) -> leviculum_ble_tx::TxFanout {
    LIVE_PEERS.lock(|peers| leviculum_ble_tx::plan_fanout(&peers.borrow(), aim))
}

/// The strict-phase clock behind
/// [`leviculum_ble_tx::SCAN_FALLBACK_AFTER_MS`], the board's one
/// [`ScanPhase`] (#375 part 2, #496).
static FALLBACK_CLOCK: BlockingMutex<CriticalSectionRawMutex, Cell<ScanPhase>> =
    BlockingMutex::new(Cell::new(ScanPhase::new()));

/// Who linked in the current phase, the "seen before" input of the
/// reset rule (#504): identities, and fixed addresses at the connection
/// event before the identity is known. Cleared when the phase reaches
/// fallback, so a peer restarts the clock at most once per phase.
static PHASE_NEIGHBOURS: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<PhaseNeighbours<PHASE_NEIGHBOUR_SLOTS>>,
> = BlockingMutex::new(RefCell::new(PhaseNeighbours::new()));
const _: () = assert!(PHASE_NEIGHBOUR_SLOTS == MAX_LINKS + DEAD_END_SLOTS);
const _: () = assert!(DEAD_END_SLOTS == 2 * MAX_LINKS);
const _: () = assert!(PERIPH_SLOTS as usize == MAX_LINKS - 1);

/// Hand one [`PhaseEvent`] to the fallback clock, which restarts the
/// strict phase iff [`PhaseEvent::resets_strict`] says so.
pub fn note_phase_event(event: PhaseEvent) {
    let now_ms = Instant::now().as_millis();
    FALLBACK_CLOCK.lock(|clock| {
        let mut phase = clock.get();
        phase.note(event, now_ms);
        clock.set(phase);
    });
}

/// The [`ScanMode`] the current strict phase has reached, logging the
/// strict-to-fallback switch once per phase (`BLE_SCAN_FALLBACK`).
pub fn scan_mode_now() -> ScanMode {
    // One lock for the read-check-mark sequence: a reset racing in
    // between two separate locks would be overwritten with the stale
    // phase. The log line itself stays outside the critical section.
    let now_ms = Instant::now().as_millis();
    let poll = FALLBACK_CLOCK.lock(|clock| {
        let mut phase = clock.get();
        let poll = phase.poll(now_ms);
        clock.set(phase);
        poll
    });
    if let Some(elapsed_ms) = poll.announce_ms {
        // The phase reached fallback: a new phase starts with nobody
        // seen, so each peer may restart the clock once more (#504).
        PHASE_NEIGHBOURS.lock(|seen| seen.borrow_mut().clear());
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_SCAN_FALLBACK after_ms={elapsed_ms}"),
        );
    }
    poll.mode
}

/// Whether the strict phase is sitting out `addr` right now (a dial the
/// peer refused buys one advertising interval, 815).
pub fn phase_skips(addr: u64, now_ms: u64) -> bool {
    FALLBACK_CLOCK.lock(|clock| clock.get().skips(addr, now_ms))
}

/// Our dial at `peer_value` ended in the peer's refusal
/// ([`leviculum_ble_tx::refused_dial`]): the strict phase lasts one
/// advertising interval from now and the address sits it out
/// ([`ScanPhase::note_refused`]).
pub fn note_dial_refused(peer_value: u64, lived_ms: u64) {
    let now_ms = Instant::now().as_millis();
    FALLBACK_CLOCK.lock(|clock| {
        let mut phase = clock.get();
        phase.note_refused(peer_value, now_ms);
        clock.set(phase);
    });
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "{}",
            DialRefusedLine {
                addr: peer_value,
                lived_ms,
            }
        ),
    );
}

/// The one dead-end table, shared by both roles (#376): a duplicate
/// refusal happens in EITHER role, and the address to back off is the
/// same address the scanner would otherwise re-offer within seconds.
static DEAD_ENDS: BlockingMutex<CriticalSectionRawMutex, RefCell<DeadEnds>> =
    BlockingMutex::new(RefCell::new(DeadEnds::new()));

/// The per-address dial backoff (579), shared by both roles like the
/// dead ends: the central books its give-ups, a peripheral session
/// books #548's handshake timeout, a handshake in either role clears
/// the address, and the scan callback reads it.
static DIAL_BACKOFF: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<DialBackoff<DIAL_BACKOFF_SLOTS>>,
> = BlockingMutex::new(RefCell::new(DialBackoff::new()));

/// Book a give-up at `addr` and say what it booked: `BLE_DIAL_BACKOFF`,
/// lnsd's line with the same keys.
pub fn note_dial_failure(addr: u64, failure: DialFailure) {
    let now_ms = Instant::now().as_millis();
    let booked = DIAL_BACKOFF.lock(|b| b.borrow_mut().note_failure(addr, failure, now_ms));
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_DIAL_BACKOFF addr={addr:012x} failure={} strikes={} backoff_ms={} until_ms={}",
            failure.as_str(),
            booked.strikes,
            booked.until_ms.saturating_sub(now_ms),
            booked.until_ms,
        ),
    );
}

/// A handshake with `addr` completed, in either role: its backoff run
/// is over (579).
pub fn note_dial_success(addr: u64) {
    DIAL_BACKOFF.lock(|b| b.borrow_mut().note_success(addr));
}

/// A sighting of `addr` in the scan callback: whether a backoff run
/// holds it right now ([`DialBackoff::sighted`]).
pub fn backoff_sighted(addr: u64, now_ms: u64) -> bool {
    DIAL_BACKOFF
        .lock(|b| b.borrow_mut().sighted(addr, now_ms))
        .is_some()
}

/// The hold to announce for a backed-off candidate the admission would
/// otherwise have dialled ([`DialBackoff::announce`]): at most one line
/// per hold.
pub fn backoff_announce(addr: u64, now_ms: u64) -> Option<leviculum_ble_tx::BackoffBooked> {
    DIAL_BACKOFF.lock(|b| b.borrow_mut().announce(addr, now_ms))
}

/// Condemn an address for [`DEAD_END_TTL_MS`] and say why on one line.
pub fn note_dead_end(addr: u64, reason: &str) {
    let now_ms = Instant::now().as_millis();
    DEAD_ENDS.lock(|table| table.borrow_mut().note(addr, now_ms));
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_DIAL_DEAD_END addr={addr:012x} reason={reason} ttl_s={}",
            DEAD_END_TTL_MS / 1_000
        ),
    );
}

/// What the board remembers about the dials it has already paid for
/// (#412 part 2), keyed by IDENTITY where the dead ends are keyed by
/// address; see [`leviculum_ble_tx::DialLedger`].
static DIAL_LEDGER: BlockingMutex<CriticalSectionRawMutex, Cell<DialLedger>> =
    BlockingMutex::new(Cell::new(DialLedger::new()));

/// The policy the dial ledger runs, and the A/B switch for it:
/// `dial-ledger-off` compiles [`LedgerPolicy::OFF`] in instead —
/// bit-identical to a board with no ledger — so a rig can run the same
/// image twice and attribute a difference to this feature alone. The
/// nRF crate forwards its `ble-dial-ledger-off` feature here.
#[cfg(not(feature = "dial-ledger-off"))]
const LEDGER_POLICY: LedgerPolicy = LedgerPolicy::MEASURED;
#[cfg(feature = "dial-ledger-off")]
const LEDGER_POLICY: LedgerPolicy = LedgerPolicy::OFF;

/// The `held_until_ms` of the last pause a suppressed dial was logged
/// for, so the `BLE_DIAL_LEDGER held` line is emitted once per PAUSE
/// and not once per advertising report.
static HELD_LOGGED_FOR: BlockingMutex<CriticalSectionRawMutex, Cell<Option<u64>>> =
    BlockingMutex::new(Cell::new(None));

/// Record one dial outcome against the identity the dial reached, and
/// say on one line when it armed the pause. `session_ms` is `None` for
/// a dial refused post-connect, `Some(ms)` for one whose link has
/// ended.
pub fn note_dial_outcome(peer_id: &[u8; 16], session_ms: Option<u64>, reason: &str) {
    let now_ms = Instant::now().as_millis();
    let noted = DIAL_LEDGER.lock(|cell| {
        let mut ledger = cell.get();
        let noted = ledger.note(LEDGER_POLICY, peer_id, session_ms, now_ms);
        cell.set(ledger);
        noted
    });
    if noted.armed {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DIAL_LEDGER armed identity={:02x}{:02x}{:02x}{:02x} run={} reason={} \
                 session_ms={} pause_ms={}",
                peer_id[0],
                peer_id[1],
                peer_id[2],
                peer_id[3],
                noted.run,
                reason,
                SessionLen(session_ms),
                LEDGER_POLICY.pause_ms,
            ),
        );
    }
}

/// Say that a FALLBACK dial is held right now, once per pause.
pub fn log_dial_held(until_ms: u64, now_ms: u64) {
    let fresh = HELD_LOGGED_FOR.lock(|cell| {
        if cell.get() == Some(until_ms) {
            return false;
        }
        cell.set(Some(until_ms));
        true
    });
    if fresh {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DIAL_LEDGER held run={} until_ms={} now_ms={}",
                LEDGER_POLICY.wasted_run, until_ms, now_ms
            ),
        );
    }
}

/// What the board remembers about the peers that keep LEAVING (#417),
/// keyed by IDENTITY like the dial ledger and fed by the opposite
/// outcome; see [`leviculum_ble_tx::ChurnLedger`].
static CHURN_LEDGER: BlockingMutex<CriticalSectionRawMutex, Cell<ChurnLedger>> =
    BlockingMutex::new(Cell::new(ChurnLedger::new()));

/// The churn policy and its A/B switch, the dial ledger's shape:
/// `churn-penalty-off` compiles [`ChurnPolicy::OFF`] in. The nRF crate
/// forwards its `ble-churn-penalty-off` feature here.
#[cfg(not(feature = "churn-penalty-off"))]
const CHURN_POLICY: ChurnPolicy = ChurnPolicy::MEASURED;
#[cfg(feature = "churn-penalty-off")]
const CHURN_POLICY: ChurnPolicy = ChurnPolicy::OFF;

/// The `held_until_ms` of the last cooldown a suppressed dial was
/// logged for — `HELD_LOGGED_FOR`'s twin.
static CHURN_HELD_LOGGED_FOR: BlockingMutex<CriticalSectionRawMutex, Cell<Option<u64>>> =
    BlockingMutex::new(Cell::new(None));

/// Record one DEPARTURE of a rotating-addressed peer (#417): the
/// identity behind `peer` ended a session by its own leaving. A
/// fixed-addressed session's end is not recorded (the class filter is
/// [`ChurnLedger::note_departure`], and reads the address type, #506).
pub fn note_peer_leave(peer_id: &[u8; 16], peer: PeerAddr, reason: &str) {
    let now_ms = Instant::now().as_millis();
    let noted = CHURN_LEDGER.lock(|cell| {
        let mut ledger = cell.get();
        let noted = ledger.note_departure(CHURN_POLICY, peer, peer_id, now_ms);
        cell.set(ledger);
        noted
    });
    let Some(noted) = noted else {
        return;
    };
    if noted.armed {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CHURN_LEDGER armed identity={:02x}{:02x}{:02x}{:02x} run={} reason={} \
                 window_ms={} cooldown_ms={}",
                peer_id[0],
                peer_id[1],
                peer_id[2],
                peer_id[3],
                noted.run,
                reason,
                CHURN_POLICY.window_ms,
                CHURN_POLICY.cooldown_ms,
            ),
        );
    }
}

/// Say that a FALLBACK dial to a ROTATING-addressed candidate is held
/// right now (#417), once per cooldown.
pub fn log_churn_held(until_ms: u64, now_ms: u64) {
    let fresh = CHURN_HELD_LOGGED_FOR.lock(|cell| {
        if cell.get() == Some(until_ms) {
            return false;
        }
        cell.set(Some(until_ms));
        true
    });
    if fresh {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_CHURN_LEDGER held run={} until_ms={} now_ms={}",
                CHURN_POLICY.leave_run, until_ms, now_ms
            ),
        );
    }
}

/// One admission verdict for a scanned advertisement, under every lock
/// it reads ([`admit`] over the registry, the dead ends and both
/// ledgers) — the scan callback's one question.
pub fn admit_scan(local: Local, adv: &AdvReport, mode: ScanMode, now_ms: u64) -> Admission {
    let dial = DIAL_LEDGER.lock(Cell::get);
    let churn = CHURN_LEDGER.lock(Cell::get);
    LIVE_PEERS.lock(|peers| {
        DEAD_ENDS.lock(|dead| {
            admit(
                local,
                adv,
                mode,
                &peers.borrow(),
                &dead.borrow(),
                Ledgers {
                    dial: &dial,
                    churn: &churn,
                },
                now_ms,
            )
        })
    })
}

/// A dial outcome's session length as the `BLE_DIAL_LEDGER` line prints
/// it: the milliseconds, or `refused` when there was no session at all.
pub struct SessionLen(pub Option<u64>);

impl core::fmt::Display for SessionLen {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("refused"),
        }
    }
}

/// A peer's advertised free-slot count as the `BLE_SCAN_DECISION` line
/// prints it: the number, or `unknown` when the peer said nothing.
pub struct FreeSlots(pub Option<u8>);

impl core::fmt::Display for FreeSlots {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(free) => write!(f, "{free}"),
            None => f.write_str("unknown"),
        }
    }
}

/// A peer's advertised identity hint as the `BLE_SCAN_DECISION` line
/// prints it (#412): the four bytes in hex, or `unknown` when the peer
/// carried none.
pub struct IdentityHint(pub Option<[u8; IDENTITY_HINT_LEN]>);

impl core::fmt::Display for IdentityHint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(h) => write!(f, "{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3]),
            None => f.write_str("unknown"),
        }
    }
}

/// Run one inbound frame through a link's defragmenter, and say so when
/// that cost a partial packet (#373). Hard reset on a garbage frame: a
/// stale head must not be left for the next packet's tail to complete
/// (#255). Mirrors the defragmenter's holding into the census
/// ([`crate::defrag_held_set`]), the one chokepoint both roles' frames
/// pass.
pub fn process_logged(
    d: &mut BleDefragmenter,
    data: &[u8],
    now_ms: u64,
    slot_index: usize,
) -> DefragResult {
    let before = d.abandoned_count();
    let result = d.process_or_abandon(data, now_ms);
    let after = d.abandoned_count();
    if after != before {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_RX_ABANDON slot={} lost={} total={}",
                slot_index,
                after.saturating_sub(before),
                after,
            ),
        );
    }
    crate::link::defrag_held_set(slot_index, d.held_bytes());
    result
}

/// Hold the next packet back until this connection's inter-packet gap
/// (#376) has elapsed: the compiled 100 ms default or the operator's
/// override ([`crate::tx_gap_ms`]). Keepalives bypass it on both sides;
/// every actual wait logs `BLE_TX_GAP`.
pub async fn serve_tx_gap(gap: &TxGap, conn_handle: u16) {
    let wait_ms = gap.wait_ms(Instant::now().as_millis(), crate::link::tx_gap_ms());
    if wait_ms > 0 {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_TX_GAP conn={} waited_ms={}", conn_handle, wait_ms),
        );
        Timer::after(Duration::from_millis(wait_ms)).await;
    }
}
