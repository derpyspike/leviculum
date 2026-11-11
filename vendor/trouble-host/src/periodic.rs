//! Periodic advertising trains and periodic advertising sync.
//!
//! A Leviculum addition to trouble-host 0.8.0 (see `LEVICULUM-PATCH.md` at
//! the crate root): the transmit side runs a periodic advertising train on
//! top of an extended advertising set ([`Peripheral::periodic_advertise`]),
//! the receive side synchronises to a train it has heard
//! ([`Central::periodic_sync`]) and reads its reports
//! ([`PeriodicSync::next_report`]). The host dispatches the three LE
//! events (`Sync Established`, `Periodic Advertising Report`, `Sync Lost`)
//! into a fixed table of [`PERIODIC_SYNCS_MAX`] slots by sync handle; a
//! report split by the controller across several `data_status` chunks is
//! reassembled into one.
//!
//! What is not here: periodic advertising sync transfer (PAST), responses
//! and subevents (PAwR), CTE. A sync that the controller reports lost
//! stays in its slot, answering [`Error::PeriodicSyncLost`], until the
//! [`PeriodicSync`] is dropped or terminated.

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Context, Poll};

use bt_hci::cmd::le::{
    LePeriodicAdvCreateSync, LePeriodicAdvCreateSyncCancel, LePeriodicAdvTerminateSync, LeSetPeriodicAdvData,
    LeSetPeriodicAdvEnable, LeSetPeriodicAdvParams,
};
use bt_hci::controller::{Controller, ControllerCmdAsync, ControllerCmdSync};
use bt_hci::param::{
    AddrKind, AdvHandle, BdAddr, CteKind, CteMask, DataStatus, LePeriodicAdvCreateSyncOptions, Operation,
    PeriodicAdvProps, PhyKind, Status, SyncHandle,
};
use embassy_sync::waitqueue::WakerRegistration;
use embassy_time::{with_timeout, Duration};
use heapless::Vec;

use crate::config::PERIODIC_SYNCS_MAX;
use crate::host::BleHost;
#[cfg(feature = "central")]
use crate::Central;
#[cfg(feature = "peripheral")]
use crate::Peripheral;
use crate::{bt_hci_duration, Address, BleHostError, Error, PacketPool};

/// The longest periodic advertising report kept, after reassembly of the
/// controller's chunks. Longer data is truncated and the report says so.
pub const PERIODIC_REPORT_MAX: usize = 255;
/// The most data one `LE Set Periodic Advertising Data` command carries.
const PERIODIC_ADV_DATA_FRAGMENT: usize = 252;

/// Parameters of a periodic advertising train.
#[derive(Clone, Copy, Debug)]
pub struct PeriodicAdvParams {
    /// The train's shortest interval, a multiple of 1.25 ms between 7.5 ms
    /// and 81.9 s.
    pub interval_min: Duration,
    /// The train's longest interval.
    pub interval_max: Duration,
    /// Put the TX power into the periodic advertisements.
    pub include_tx_power: bool,
}

impl Default for PeriodicAdvParams {
    fn default() -> Self {
        Self {
            interval_min: Duration::from_millis(240),
            interval_max: Duration::from_millis(240),
            include_tx_power: false,
        }
    }
}

/// What to sync to and how long to wait for it.
#[derive(Clone, Copy, Debug)]
pub struct PeriodicSyncConfig {
    /// The advertiser's address, as its extended advertisement carried it.
    pub advertiser: Address,
    /// The advertising SID of the train.
    pub sid: u8,
    /// Periodic advertising events the controller may skip after one it
    /// received (0: none).
    pub skip: u16,
    /// The synchronisation timeout: the sync is lost after this long
    /// without a received event (100 ms to 163.84 s).
    pub timeout: Duration,
    /// How long [`Central::periodic_sync`] waits for the establishment
    /// before cancelling and returning [`Error::Timeout`].
    pub create_timeout: Duration,
}

/// What the controller said when the sync was established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncInfo {
    /// The controller's handle for the sync.
    pub handle: SyncHandle,
    /// The train's advertiser.
    pub advertiser: Address,
    /// The train's advertising SID.
    pub sid: u8,
    /// The PHY the train uses.
    pub phy: PhyKind,
    /// The train's interval as the advertiser runs it.
    pub interval: Duration,
}

/// One periodic advertising event, reassembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeriodicReport {
    /// The event's data, every chunk of it.
    pub data: Vec<u8, PERIODIC_REPORT_MAX>,
    /// RSSI in dBm (127: not available).
    pub rssi: i8,
    /// TX power in dBm (127: not available).
    pub tx_power: i8,
    /// The constant tone extension the event carried, if any.
    pub cte: CteKind,
    /// The controller's data did not fit [`PERIODIC_REPORT_MAX`].
    pub truncated: bool,
}

/// Counters of the sync table, for a host's diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeriodicCounters {
    /// Reports completed and handed to a slot.
    pub reports: u32,
    /// Reports that overwrote one nobody had read yet.
    pub reports_dropped: u32,
    /// Reports the controller marked failed (their chunks discarded).
    pub reports_failed: u32,
    /// Reports truncated to [`PERIODIC_REPORT_MAX`].
    pub reports_truncated: u32,
    /// Events for a sync handle no slot holds.
    pub unknown_handle: u32,
    /// Syncs the controller established.
    pub syncs_established: u32,
    /// Creates the controller answered with an error.
    pub syncs_failed: u32,
    /// Syncs the controller lost.
    pub syncs_lost: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free,
    /// `LE Periodic Advertising Create Sync` is out; waiting for the
    /// establishment.
    Creating,
    /// The waiting future was dropped: the runner is to cancel the create.
    CancelRequested,
    /// The cancel command was sent; the establishment event (cancelled or,
    /// if it raced, established) frees or terminates the slot.
    Cancelling,
    Established(SyncHandle),
    /// The `PeriodicSync` was dropped: the runner is to terminate it.
    TerminateRequested(SyncHandle),
    /// The controller lost it; the slot answers until it is released.
    Lost,
}

struct Slot {
    state: SlotState,
    advertiser: Address,
    sid: u8,
    outcome: Option<Result<SyncInfo, bt_hci::param::Error>>,
    report: Option<PeriodicReport>,
    assembling: Vec<u8, PERIODIC_REPORT_MAX>,
    assembling_truncated: bool,
    waker: WakerRegistration,
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: SlotState::Free,
            advertiser: Address {
                kind: AddrKind::PUBLIC,
                addr: BdAddr([0; 6]),
            },
            sid: 0,
            outcome: None,
            report: None,
            assembling: Vec::new(),
            assembling_truncated: false,
            waker: WakerRegistration::new(),
        }
    }

    fn handle(&self) -> Option<SyncHandle> {
        match self.state {
            SlotState::Established(h) | SlotState::TerminateRequested(h) => Some(h),
            _ => None,
        }
    }

    fn clear(&mut self) {
        self.state = SlotState::Free;
        self.outcome = None;
        self.report = None;
        self.assembling.clear();
        self.assembling_truncated = false;
        self.waker.wake();
    }
}

/// What the runner has to send on someone's behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingAction {
    CancelCreate(u8),
    Terminate(u8, SyncHandle),
    StopAdvertising(AdvHandle),
}

/// The sync table and the runner's to-do list; lives in the host state.
pub(crate) struct PeriodicState {
    slots: RefCell<[Slot; PERIODIC_SYNCS_MAX]>,
    adv_stop: RefCell<Vec<AdvHandle, 4>>,
    runner_waker: RefCell<WakerRegistration>,
    counters: RefCell<PeriodicCounters>,
}

impl PeriodicState {
    pub(crate) const fn new() -> Self {
        Self {
            slots: RefCell::new([const { Slot::new() }; PERIODIC_SYNCS_MAX]),
            adv_stop: RefCell::new(Vec::new()),
            runner_waker: RefCell::new(WakerRegistration::new()),
            counters: RefCell::new(PeriodicCounters {
                reports: 0,
                reports_dropped: 0,
                reports_failed: 0,
                reports_truncated: 0,
                unknown_handle: 0,
                syncs_established: 0,
                syncs_failed: 0,
                syncs_lost: 0,
            }),
        }
    }

    pub(crate) fn counters(&self) -> PeriodicCounters {
        *self.counters.borrow()
    }

    /// Syncs held (established or lost-but-unreleased).
    pub(crate) fn held(&self) -> usize {
        self.slots
            .borrow()
            .iter()
            .filter(|s| !matches!(s.state, SlotState::Free))
            .count()
    }

    /// Take a slot for a create. One create may be pending at a time
    /// (the controller allows one), a train is held once, and the table
    /// is finite.
    pub(crate) fn claim(&self, advertiser: Address, sid: u8) -> Result<u8, Error> {
        let mut slots = self.slots.borrow_mut();
        if slots.iter().any(|s| {
            matches!(
                s.state,
                SlotState::Creating | SlotState::CancelRequested | SlotState::Cancelling
            )
        }) {
            return Err(Error::Busy);
        }
        if slots
            .iter()
            .any(|s| !matches!(s.state, SlotState::Free) && s.advertiser == advertiser && s.sid == sid)
        {
            return Err(Error::InvalidState);
        }
        let Some(index) = slots.iter().position(|s| matches!(s.state, SlotState::Free)) else {
            return Err(Error::InsufficientSpace);
        };
        let slot = &mut slots[index];
        slot.clear();
        slot.state = SlotState::Creating;
        slot.advertiser = advertiser;
        slot.sid = sid;
        Ok(index as u8)
    }

    pub(crate) fn free(&self, index: u8) {
        self.slots.borrow_mut()[index as usize].clear();
    }

    fn wake_runner(&self) {
        self.runner_waker.borrow_mut().wake();
    }

    pub(crate) fn request_cancel_create(&self, index: u8) {
        let mut slots = self.slots.borrow_mut();
        let slot = &mut slots[index as usize];
        match slot.state {
            SlotState::Creating => {
                slot.state = SlotState::CancelRequested;
                drop(slots);
                self.wake_runner();
            }
            // Established while the waiter was being dropped: terminate.
            SlotState::Established(h) => {
                slot.state = SlotState::TerminateRequested(h);
                drop(slots);
                self.wake_runner();
            }
            _ => {}
        }
    }

    pub(crate) fn request_terminate(&self, index: u8) {
        let mut slots = self.slots.borrow_mut();
        let slot = &mut slots[index as usize];
        match slot.state {
            SlotState::Established(h) => {
                slot.state = SlotState::TerminateRequested(h);
                drop(slots);
                self.wake_runner();
            }
            // Nothing to terminate at the controller.
            SlotState::Lost => slot.clear(),
            _ => {}
        }
    }

    pub(crate) fn request_adv_stop(&self, handle: AdvHandle) {
        let _ = self.adv_stop.borrow_mut().push(handle);
        self.wake_runner();
    }

    /// The runner's side: the next thing to send, if any.
    pub(crate) fn poll_pending(&self, cx: &mut Context<'_>) -> Poll<PendingAction> {
        self.runner_waker.borrow_mut().register(cx.waker());
        if let Some(h) = self.adv_stop.borrow_mut().pop() {
            return Poll::Ready(PendingAction::StopAdvertising(h));
        }
        let slots = self.slots.borrow();
        for (i, s) in slots.iter().enumerate() {
            match s.state {
                SlotState::CancelRequested => return Poll::Ready(PendingAction::CancelCreate(i as u8)),
                SlotState::TerminateRequested(h) => return Poll::Ready(PendingAction::Terminate(i as u8, h)),
                _ => {}
            }
        }
        Poll::Pending
    }

    /// The runner sent the cancel: the establishment event settles the slot.
    pub(crate) fn cancel_sent(&self, index: u8) {
        let mut slots = self.slots.borrow_mut();
        if matches!(slots[index as usize].state, SlotState::CancelRequested) {
            slots[index as usize].state = SlotState::Cancelling;
        }
    }

    /// `LE Periodic Advertising Sync Established`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_established(
        &self,
        status: Status,
        handle: SyncHandle,
        sid: u8,
        addr_kind: AddrKind,
        addr: BdAddr,
        phy: PhyKind,
        interval: Duration,
    ) {
        let mut slots = self.slots.borrow_mut();
        let advertiser = Address { kind: addr_kind, addr };
        // The pending create; the controller allows one, and a cancelled
        // one answers with this event too.
        let Some(slot) = slots.iter_mut().find(|s| {
            matches!(
                s.state,
                SlotState::Creating | SlotState::CancelRequested | SlotState::Cancelling
            )
        }) else {
            self.counters.borrow_mut().unknown_handle += 1;
            return;
        };
        let cancelling = matches!(slot.state, SlotState::CancelRequested | SlotState::Cancelling);
        match status.to_result() {
            Ok(()) => {
                let info = SyncInfo {
                    handle,
                    advertiser,
                    sid,
                    phy,
                    interval,
                };
                self.counters.borrow_mut().syncs_established += 1;
                if cancelling {
                    // Established before the cancel took: nobody waits for
                    // it, so it goes straight to termination.
                    slot.state = SlotState::TerminateRequested(handle);
                    drop(slots);
                    self.wake_runner();
                } else {
                    slot.state = SlotState::Established(handle);
                    slot.outcome = Some(Ok(info));
                    slot.waker.wake();
                }
            }
            Err(e) => {
                if cancelling {
                    slot.clear();
                } else {
                    self.counters.borrow_mut().syncs_failed += 1;
                    slot.outcome = Some(Err(e));
                    slot.waker.wake();
                }
            }
        }
    }

    /// `LE Periodic Advertising Report`, one chunk of it.
    pub(crate) fn on_report(
        &self,
        handle: SyncHandle,
        tx_power: i8,
        rssi: i8,
        cte: CteKind,
        data_status: DataStatus,
        data: &[u8],
    ) {
        let mut slots = self.slots.borrow_mut();
        let Some(slot) = slots.iter_mut().find(|s| s.handle() == Some(handle)) else {
            self.counters.borrow_mut().unknown_handle += 1;
            return;
        };
        if data_status == DataStatus::Failed {
            slot.assembling.clear();
            slot.assembling_truncated = false;
            self.counters.borrow_mut().reports_failed += 1;
            return;
        }
        let room = PERIODIC_REPORT_MAX - slot.assembling.len();
        if data.len() > room {
            slot.assembling_truncated = true;
        }
        let _ = slot.assembling.extend_from_slice(&data[..data.len().min(room)]);
        if data_status == DataStatus::Incomplete {
            return;
        }
        let mut counters = self.counters.borrow_mut();
        counters.reports += 1;
        if slot.assembling_truncated {
            counters.reports_truncated += 1;
        }
        if slot.report.is_some() {
            counters.reports_dropped += 1;
        }
        slot.report = Some(PeriodicReport {
            data: core::mem::take(&mut slot.assembling),
            rssi,
            tx_power,
            cte,
            truncated: core::mem::take(&mut slot.assembling_truncated),
        });
        slot.waker.wake();
    }

    /// `LE Periodic Advertising Sync Lost`.
    pub(crate) fn on_lost(&self, handle: SyncHandle) {
        let mut slots = self.slots.borrow_mut();
        let Some(slot) = slots.iter_mut().find(|s| s.handle() == Some(handle)) else {
            self.counters.borrow_mut().unknown_handle += 1;
            return;
        };
        self.counters.borrow_mut().syncs_lost += 1;
        match slot.state {
            // Nobody will read it: the controller has dropped it already.
            SlotState::TerminateRequested(_) => slot.clear(),
            _ => {
                slot.state = SlotState::Lost;
                slot.waker.wake();
            }
        }
    }

    /// The creating side: the establishment outcome.
    pub(crate) fn poll_established(
        &self,
        index: u8,
        cx: &mut Context<'_>,
    ) -> Poll<Result<SyncInfo, bt_hci::param::Error>> {
        let mut slots = self.slots.borrow_mut();
        let slot = &mut slots[index as usize];
        slot.waker.register(cx.waker());
        match slot.outcome.take() {
            Some(o) => Poll::Ready(o),
            None => Poll::Pending,
        }
    }

    /// The holder's side: the next report, or that the sync is gone.
    pub(crate) fn poll_report(&self, index: u8, cx: &mut Context<'_>) -> Poll<Result<PeriodicReport, Error>> {
        let mut slots = self.slots.borrow_mut();
        let slot = &mut slots[index as usize];
        slot.waker.register(cx.waker());
        if let Some(r) = slot.report.take() {
            return Poll::Ready(Ok(r));
        }
        match slot.state {
            SlotState::Established(_) => Poll::Pending,
            _ => Poll::Ready(Err(Error::PeriodicSyncLost)),
        }
    }
}

/// A sync to a periodic train; drop it or [`terminate`](Self::terminate)
/// it to release the controller's sync.
pub struct PeriodicSync<'d, C, P: PacketPool> {
    host: BleHost<'d, C, P>,
    index: u8,
    /// What the controller said when it established the sync.
    pub info: SyncInfo,
    done: bool,
}

impl<'d, C: Controller, P: PacketPool> PeriodicSync<'d, C, P> {
    /// The controller's handle for the sync.
    pub fn handle(&self) -> SyncHandle {
        self.info.handle
    }

    /// The next reassembled periodic advertising event; [`Error::PeriodicSyncLost`]
    /// once the controller has lost the sync.
    pub async fn next_report(&mut self) -> Result<PeriodicReport, Error> {
        poll_fn(|cx| self.host.periodic().poll_report(self.index, cx)).await
    }

    /// Release the sync at the controller now.
    pub async fn terminate(mut self) -> Result<(), BleHostError<C::Error>>
    where
        C: ControllerCmdSync<LePeriodicAdvTerminateSync>,
    {
        self.done = true;
        let result = self.host.command(LePeriodicAdvTerminateSync::new(self.info.handle)).await;
        self.host.periodic().free(self.index);
        result.map(|_| ())
    }
}

impl<C, P: PacketPool> Drop for PeriodicSync<'_, C, P> {
    fn drop(&mut self) {
        if !self.done {
            self.host.periodic().request_terminate(self.index);
        }
    }
}

#[cfg(feature = "central")]
impl<'stack, C: Controller, P: PacketPool> Central<'stack, C, P> {
    /// Synchronise to a periodic train. Waits at most `create_timeout`;
    /// dropping the future cancels the create at the controller.
    pub async fn periodic_sync(
        &mut self,
        config: &PeriodicSyncConfig,
    ) -> Result<PeriodicSync<'stack, C, P>, BleHostError<C::Error>>
    where
        C: ControllerCmdAsync<LePeriodicAdvCreateSync> + ControllerCmdSync<LePeriodicAdvCreateSyncCancel>,
    {
        let host = self.host;
        let index = host.periodic().claim(config.advertiser, config.sid)?;
        let guard = crate::host::OnDrop::new(|| host.periodic().request_cancel_create(index));
        let sent = host
            .async_command(LePeriodicAdvCreateSync::new(
                LePeriodicAdvCreateSyncOptions::new(),
                config.sid,
                config.advertiser.kind,
                config.advertiser.addr,
                config.skip,
                bt_hci_duration(config.timeout),
                CteMask::new(),
            ))
            .await;
        if let Err(e) = sent {
            guard.defuse();
            host.periodic().free(index);
            return Err(e);
        }
        let outcome = with_timeout(
            config.create_timeout,
            poll_fn(|cx| host.periodic().poll_established(index, cx)),
        )
        .await;
        guard.defuse();
        match outcome {
            Ok(Ok(info)) => Ok(PeriodicSync {
                host,
                index,
                info,
                done: false,
            }),
            Ok(Err(e)) => {
                host.periodic().free(index);
                Err(Error::Hci(e).into())
            }
            Err(_) => {
                // Cancel here rather than through the runner: this future
                // is still alive to await it. The cancelled establishment
                // event then finds no pending slot and is counted.
                let _ = host.command(LePeriodicAdvCreateSyncCancel::new()).await;
                host.periodic().free(index);
                Err(Error::Timeout.into())
            }
        }
    }
}

/// A running periodic advertising train on an extended advertising set;
/// drop it or [`stop`](Self::stop) it to disable the train.
pub struct PeriodicAdvertiser<'d, C, P: PacketPool> {
    host: BleHost<'d, C, P>,
    handle: AdvHandle,
    done: bool,
}

async fn set_periodic_data<C, P: PacketPool>(
    host: &BleHost<'_, C, P>,
    handle: AdvHandle,
    data: &[u8],
) -> Result<(), BleHostError<C::Error>>
where
    C: Controller + for<'t> ControllerCmdSync<LeSetPeriodicAdvData<'t>>,
{
    if data.len() <= PERIODIC_ADV_DATA_FRAGMENT {
        host.command(LeSetPeriodicAdvData::new(handle, Operation::Complete, data))
            .await?;
        return Ok(());
    }
    let chunks = data.chunks(PERIODIC_ADV_DATA_FRAGMENT);
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.enumerate() {
        let op = if i == 0 {
            Operation::FirstFragment
        } else if i == last {
            Operation::LastFragment
        } else {
            Operation::IntermediateFragment
        };
        host.command(LeSetPeriodicAdvData::new(handle, op, chunk)).await?;
    }
    Ok(())
}

#[cfg(feature = "peripheral")]
impl<'d, C: Controller, P: PacketPool> Peripheral<'d, C, P> {
    /// Start a periodic train on the extended advertising set `adv_handle`
    /// (from the `AdvSet` handles [`Peripheral::advertise_ext`] filled) with
    /// `data` in every periodic event.
    pub async fn periodic_advertise(
        &mut self,
        adv_handle: AdvHandle,
        params: &PeriodicAdvParams,
        data: &[u8],
    ) -> Result<PeriodicAdvertiser<'d, C, P>, BleHostError<C::Error>>
    where
        C: ControllerCmdSync<LeSetPeriodicAdvParams>
            + for<'t> ControllerCmdSync<LeSetPeriodicAdvData<'t>>
            + ControllerCmdSync<LeSetPeriodicAdvEnable>,
    {
        let host = self.host;
        host.command(LeSetPeriodicAdvParams::new(
            adv_handle,
            bt_hci_duration(params.interval_min),
            bt_hci_duration(params.interval_max),
            PeriodicAdvProps::new().include_tx_power(params.include_tx_power),
        ))
        .await?;
        set_periodic_data(&host, adv_handle, data).await?;
        host.command(LeSetPeriodicAdvEnable::new(true, adv_handle)).await?;
        Ok(PeriodicAdvertiser {
            host,
            handle: adv_handle,
            done: false,
        })
    }
}

impl<'d, C: Controller, P: PacketPool> PeriodicAdvertiser<'d, C, P> {
    /// The extended advertising set the train rides on.
    pub fn handle(&self) -> AdvHandle {
        self.handle
    }

    /// Replace the data of the following periodic events.
    pub async fn update(&mut self, data: &[u8]) -> Result<(), BleHostError<C::Error>>
    where
        C: for<'t> ControllerCmdSync<LeSetPeriodicAdvData<'t>>,
    {
        set_periodic_data(&self.host, self.handle, data).await
    }

    /// Disable the train now.
    pub async fn stop(mut self) -> Result<(), BleHostError<C::Error>>
    where
        C: ControllerCmdSync<LeSetPeriodicAdvEnable>,
    {
        self.done = true;
        self.host
            .command(LeSetPeriodicAdvEnable::new(false, self.handle))
            .await
            .map(|_| ())
    }
}

impl<C, P: PacketPool> Drop for PeriodicAdvertiser<'_, C, P> {
    fn drop(&mut self) {
        if !self.done {
            self.host.periodic().request_adv_stop(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::task::Waker;

    use super::*;

    fn addr(n: u8) -> Address {
        Address {
            kind: AddrKind::RANDOM,
            addr: BdAddr::new([n, 1, 2, 3, 4, 5]),
        }
    }

    fn establish(state: &PeriodicState, index: u8, handle: u16) -> SyncInfo {
        let a = {
            let slots = state.slots.borrow();
            (slots[index as usize].advertiser, slots[index as usize].sid)
        };
        state.on_established(
            Status::SUCCESS,
            SyncHandle(handle),
            a.1,
            a.0.kind,
            a.0.addr,
            PhyKind::Le1M,
            Duration::from_millis(240),
        );
        let mut cx = Context::from_waker(Waker::noop());
        match state.poll_established(index, &mut cx) {
            Poll::Ready(Ok(info)) => info,
            other => panic!("not established: {:?}", other.is_ready()),
        }
    }

    #[test]
    fn a_sync_is_claimed_established_reported_and_lost() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 3).unwrap();
        assert!(state.poll_established(index, &mut cx).is_pending());
        let info = establish(&state, index, 0x10);
        assert_eq!(info.handle, SyncHandle(0x10));
        assert_eq!(info.sid, 3);
        assert_eq!(state.held(), 1);
        assert!(state.poll_report(index, &mut cx).is_pending());
        state.on_report(SyncHandle(0x10), 4, -60, CteKind::NoCte, DataStatus::Complete, &[1, 2, 3]);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => {
                assert_eq!(r.data.as_slice(), &[1, 2, 3]);
                assert_eq!((r.rssi, r.tx_power, r.truncated), (-60, 4, false));
            }
            _ => panic!("no report"),
        }
        assert!(state.poll_report(index, &mut cx).is_pending());
        state.on_lost(SyncHandle(0x10));
        assert!(matches!(
            state.poll_report(index, &mut cx),
            Poll::Ready(Err(Error::PeriodicSyncLost))
        ));
        assert_eq!(state.counters().syncs_lost, 1);
        // A lost sync needs no terminate at the controller: releasing it
        // frees the slot without a runner action.
        state.request_terminate(index);
        assert_eq!(state.held(), 0);
        assert!(state.poll_pending(&mut cx).is_pending());
    }

    #[test]
    fn a_report_split_in_chunks_is_reassembled_and_a_failed_one_discarded() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        establish(&state, index, 7);
        let h = SyncHandle(7);
        state.on_report(h, 0, -50, CteKind::NoCte, DataStatus::Incomplete, &[1, 2]);
        assert!(state.poll_report(index, &mut cx).is_pending());
        state.on_report(h, 0, -50, CteKind::NoCte, DataStatus::Incomplete, &[3]);
        state.on_report(h, 0, -51, CteKind::NoCte, DataStatus::Complete, &[4, 5]);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => {
                assert_eq!(r.data.as_slice(), &[1, 2, 3, 4, 5]);
                assert_eq!(r.rssi, -51);
            }
            _ => panic!("no report"),
        }
        // Failed in the middle: the chunks so far are dropped and the
        // next complete report starts clean.
        state.on_report(h, 0, -50, CteKind::NoCte, DataStatus::Incomplete, &[9, 9]);
        state.on_report(h, 0, -50, CteKind::NoCte, DataStatus::Failed, &[]);
        assert!(state.poll_report(index, &mut cx).is_pending());
        state.on_report(h, 0, -50, CteKind::NoCte, DataStatus::Complete, &[6]);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => assert_eq!(r.data.as_slice(), &[6]),
            _ => panic!("no report"),
        }
        let c = state.counters();
        assert_eq!((c.reports, c.reports_failed), (2, 1));
    }

    #[test]
    fn a_report_longer_than_the_buffer_is_truncated_and_says_so() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        establish(&state, index, 1);
        let big = [0xABu8; 200];
        state.on_report(SyncHandle(1), 0, 0, CteKind::NoCte, DataStatus::Incomplete, &big);
        state.on_report(SyncHandle(1), 0, 0, CteKind::NoCte, DataStatus::Complete, &big);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => {
                assert_eq!(r.data.len(), PERIODIC_REPORT_MAX);
                assert!(r.truncated);
            }
            _ => panic!("no report"),
        }
        assert_eq!(state.counters().reports_truncated, 1);
        // Negative control: one that fits is not marked.
        state.on_report(SyncHandle(1), 0, 0, CteKind::NoCte, DataStatus::Complete, &big);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => assert!(!r.truncated),
            _ => panic!("no report"),
        }
    }

    #[test]
    fn an_unread_report_is_overwritten_and_counted() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        establish(&state, index, 1);
        state.on_report(SyncHandle(1), 0, 0, CteKind::NoCte, DataStatus::Complete, &[1]);
        state.on_report(SyncHandle(1), 0, 0, CteKind::NoCte, DataStatus::Complete, &[2]);
        match state.poll_report(index, &mut cx) {
            Poll::Ready(Ok(r)) => assert_eq!(r.data.as_slice(), &[2]),
            _ => panic!("no report"),
        }
        assert_eq!(state.counters().reports_dropped, 1);
    }

    #[test]
    fn the_table_holds_its_maximum_and_refuses_the_next_and_a_duplicate() {
        let state = PeriodicState::new();
        for i in 0..PERIODIC_SYNCS_MAX as u8 {
            let index = state.claim(addr(i), 0).unwrap();
            establish(&state, index, 0x100 + u16::from(i));
        }
        assert_eq!(state.held(), PERIODIC_SYNCS_MAX);
        assert_eq!(state.claim(addr(0x40), 0), Err(Error::InsufficientSpace));
        // The same train again is a duplicate before it is a capacity
        // question; a second SID of the same advertiser is another train.
        assert_eq!(state.claim(addr(0), 0), Err(Error::InvalidState));
        state.on_lost(SyncHandle(0x100));
        state.request_terminate(0);
        assert_eq!(state.claim(addr(1), 0), Err(Error::InvalidState));
        assert!(state.claim(addr(1), 1).is_ok());
    }

    #[test]
    fn one_create_at_a_time() {
        let state = PeriodicState::new();
        let _first = state.claim(addr(1), 0).unwrap();
        assert_eq!(state.claim(addr(2), 0), Err(Error::Busy));
    }

    #[test]
    fn events_for_an_unknown_handle_are_counted_and_ignored() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        establish(&state, index, 5);
        state.on_report(SyncHandle(6), 0, 0, CteKind::NoCte, DataStatus::Complete, &[1]);
        state.on_lost(SyncHandle(6));
        assert!(state.poll_report(index, &mut cx).is_pending());
        assert_eq!(state.counters().unknown_handle, 2);
        // An establishment with nothing pending (a cancelled create whose
        // waiter already gave up) is counted the same way.
        state.on_established(
            Status::SUCCESS,
            SyncHandle(9),
            0,
            AddrKind::RANDOM,
            BdAddr::new([9; 6]),
            PhyKind::Le1M,
            Duration::from_millis(100),
        );
        assert_eq!(state.counters().unknown_handle, 3);
        assert_eq!(state.held(), 1);
    }

    #[test]
    fn a_failed_establishment_reports_the_status_and_keeps_the_slot_for_the_waiter() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        state.on_established(
            Status::from(0x3e),
            SyncHandle(0),
            0,
            AddrKind::RANDOM,
            BdAddr::new([1, 1, 2, 3, 4, 5]),
            PhyKind::Le1M,
            Duration::from_millis(100),
        );
        assert!(matches!(state.poll_established(index, &mut cx), Poll::Ready(Err(_))));
        assert_eq!(state.counters().syncs_failed, 1);
        state.free(index);
        assert_eq!(state.held(), 0);
    }

    #[test]
    fn a_dropped_waiter_asks_the_runner_to_cancel_and_the_event_settles_it() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        assert!(state.poll_pending(&mut cx).is_pending());
        state.request_cancel_create(index);
        assert_eq!(state.poll_pending(&mut cx), Poll::Ready(PendingAction::CancelCreate(index)));
        state.cancel_sent(index);
        // The controller answers the cancel with a failed establishment:
        // the slot is free, nothing counted as a failure.
        state.on_established(
            Status::from(0x44),
            SyncHandle(0),
            0,
            AddrKind::RANDOM,
            BdAddr::new([1, 1, 2, 3, 4, 5]),
            PhyKind::Le1M,
            Duration::from_millis(100),
        );
        assert_eq!(state.held(), 0);
        assert_eq!(state.counters().syncs_failed, 0);
        // The race: established before the cancel took. The slot goes to
        // termination on its own.
        let index = state.claim(addr(2), 0).unwrap();
        state.request_cancel_create(index);
        state.cancel_sent(index);
        state.on_established(
            Status::SUCCESS,
            SyncHandle(0x20),
            0,
            AddrKind::RANDOM,
            BdAddr::new([2, 1, 2, 3, 4, 5]),
            PhyKind::Le1M,
            Duration::from_millis(100),
        );
        assert_eq!(
            state.poll_pending(&mut cx),
            Poll::Ready(PendingAction::Terminate(index, SyncHandle(0x20)))
        );
        state.free(index);
        assert_eq!(state.held(), 0);
    }

    #[test]
    fn a_dropped_sync_asks_the_runner_to_terminate_and_a_dropped_train_to_stop() {
        let state = PeriodicState::new();
        let mut cx = Context::from_waker(Waker::noop());
        let index = state.claim(addr(1), 0).unwrap();
        establish(&state, index, 0x30);
        state.request_terminate(index);
        assert_eq!(
            state.poll_pending(&mut cx),
            Poll::Ready(PendingAction::Terminate(index, SyncHandle(0x30)))
        );
        // A report for a sync being terminated still lands (the handle is
        // live until the command goes out) and is dropped with the slot.
        state.on_report(SyncHandle(0x30), 0, 0, CteKind::NoCte, DataStatus::Complete, &[1]);
        state.free(index);
        assert_eq!(state.held(), 0);
        state.request_adv_stop(AdvHandle::new(2));
        assert_eq!(
            state.poll_pending(&mut cx),
            Poll::Ready(PendingAction::StopAdvertising(AdvHandle::new(2)))
        );
        assert!(state.poll_pending(&mut cx).is_pending());
    }
}
