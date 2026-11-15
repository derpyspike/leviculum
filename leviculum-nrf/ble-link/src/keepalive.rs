//! The keepalive deadline of one link's outbound loop, armed once and
//! re-armed only when its due instant moves (order 1726).
//!
//! An embassy [`Timer`] answers its first poll `Pending` however late it
//! is, and only a timer that has been polled once can answer `Ready`. An
//! outbound loop that rebuilt `Timer::at(last_keepalive + interval)` on
//! every pass of its `select` therefore never sent a keepalive while the
//! other arm was ready at every pass: each fresh timer was polled at most
//! once. On the M9, whose core-0 loop polls every future of ours with a
//! no-op waker (`docs/src/concepts/thinknode-m9.md`, "esp-rtos
//! exception"), that is a link whose keepalive starves behind a busy
//! queue; under an executor the same loop starved the same way, since
//! the executor polls the same `select` in the same order.
//!
//! [`KeepaliveClock`] keeps ONE timer across the passes and builds a new
//! one only when [`due`] changes, which happens when a keepalive went out
//! (`last_keepalive` moved) or the link's TX hold opened or closed. And it
//! polls the deadline BEFORE the other arm, so a due keepalive is never
//! shadowed by a packet that is ready at the same pass: `select` stops at
//! the first ready arm and never polls the rest.

use core::future::Future;

use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use leviculum_core::framing::ble::KEEPALIVE_INTERVAL_MS;

/// When the next keepalive is due: [`KEEPALIVE_INTERVAL_MS`] after the
/// last one while the link may send, never while its TX hold is shut
/// (before the CCCD subscription a keepalive notify fails exactly as the
/// first packet does, #376).
pub fn due(last_keepalive: Instant, ready: bool) -> Instant {
    if ready {
        last_keepalive + Duration::from_millis(KEEPALIVE_INTERVAL_MS)
    } else {
        Instant::MAX
    }
}

/// One link's keepalive deadline, alive for the whole outbound loop.
pub struct KeepaliveClock {
    due: Instant,
    timer: Timer,
}

impl Default for KeepaliveClock {
    fn default() -> Self {
        Self::new()
    }
}

impl KeepaliveClock {
    /// Unarmed: due never.
    pub fn new() -> Self {
        Self {
            due: Instant::MAX,
            timer: Timer::at(Instant::MAX),
        }
    }

    /// One pass of the outbound loop: `None` when the keepalive is due,
    /// `Some(output)` when `other` finished first. The timer is rebuilt
    /// only when `due` differs from the instant it was armed for.
    pub async fn next<F: Future>(&mut self, due: Instant, other: F) -> Option<F::Output> {
        if due != self.due {
            self.due = due;
            self.timer = Timer::at(due);
        }
        match select(&mut self.timer, other).await {
            Either::First(()) => None,
            Either::Second(output) => Some(output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use embassy_time::MockDriver;

    /// The mock clock is global to the test binary: the tests here that
    /// move it take this lock so their deadlines stay their own.
    static CLOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    extern crate std;

    /// One loop pass, polled once with a no-op waker as the M9's core-0
    /// loop polls it: `Some(true)` keepalive, `Some(false)` packet,
    /// `None` still pending.
    fn pass<F: Future>(clock: &mut KeepaliveClock, due: Instant, other: F) -> Option<bool> {
        let mut cx = Context::from_waker(Waker::noop());
        match pin!(clock.next(due, other)).poll(&mut cx) {
            Poll::Ready(None) => Some(true),
            Poll::Ready(Some(_)) => Some(false),
            Poll::Pending => None,
        }
    }

    /// An outbound stream with a packet ready at every pass, one pass a
    /// turn, the clock moving one turn between passes: the keepalive
    /// still goes out within the interval plus one turn of being last
    /// sent. A loop that builds a fresh timer per pass, or polls the
    /// packet arm first, sends it never.
    #[test]
    fn an_always_ready_stream_does_not_starve_the_keepalive() {
        let _clock = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
        const TURN_MS: u64 = 10;
        let driver = MockDriver::get();
        let mut clock = KeepaliveClock::new();
        let mut last_keepalive = Instant::now();
        let mut sent = 0;
        let mut turns_since = 0u64;
        for _ in 0..(3 * KEEPALIVE_INTERVAL_MS / TURN_MS) {
            match pass(
                &mut clock,
                due(last_keepalive, true),
                core::future::ready(()),
            ) {
                Some(true) => {
                    assert!(
                        turns_since * TURN_MS <= KEEPALIVE_INTERVAL_MS + TURN_MS,
                        "keepalive {} ms after the last one",
                        turns_since * TURN_MS
                    );
                    sent += 1;
                    turns_since = 0;
                    last_keepalive = Instant::now();
                }
                Some(false) => {}
                None => panic!("an always-ready stream left the pass pending"),
            }
            driver.advance(Duration::from_millis(TURN_MS));
            turns_since += 1;
        }
        assert!(sent >= 2, "{sent} keepalives in three intervals");
    }

    /// While the TX hold is shut nothing is due however long the link
    /// waits, and the deadline counts from the last keepalive once it
    /// opens.
    #[test]
    fn a_shut_hold_sends_no_keepalive() {
        let _clock = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
        let driver = MockDriver::get();
        let mut clock = KeepaliveClock::new();
        let last_keepalive = Instant::now();
        for _ in 0..4 {
            assert_eq!(
                pass(
                    &mut clock,
                    due(last_keepalive, false),
                    core::future::pending::<()>()
                ),
                None
            );
            driver.advance(Duration::from_millis(KEEPALIVE_INTERVAL_MS));
        }
        // Open: long overdue, so due at the second poll of the new timer.
        assert_eq!(
            pass(
                &mut clock,
                due(last_keepalive, true),
                core::future::pending::<()>()
            ),
            None
        );
        assert_eq!(
            pass(
                &mut clock,
                due(last_keepalive, true),
                core::future::pending::<()>()
            ),
            Some(true)
        );
    }
}
