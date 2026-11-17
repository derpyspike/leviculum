//! The owner checks and repairs the path to its own destination (reviewer
//! decision 2026-11-16, ending the transport exemptions of orders 1881 to
//! 1931).
//!
//! A program that announces a destination through a shared instance — the
//! Loquax home or mailbox, lnpnd, any leviculum-std client — asks its
//! daemon at a modest beat where the path to its OWN destination points,
//! over the shared-instance RPC calls `rnpath` uses: `next_hop` and
//! `next_hop_if_name`, which both daemons serve (ours:
//! `leviculum-std/src/rpc/handlers.rs#get_next_hop` and
//! `#get_next_hop_if_name`; Python rnsd: `reference/Reticulum/RNS/Reticulum.py:1222-1223`
//! serving, `:1597-1630` asking). If the path is not the client's own local
//! attachment, it announces at once: a newer emission wins by the ordinary
//! rule on every stack, and only the holder of the destination's key can
//! make one. A replayer can at most trigger one extra announce per rate
//! window.
//!
//! The daemon is never asked to trust anyone: this is client behaviour
//! above the interface, in the shared-instance client of leviculum-std.
//! The transport stays Python's, and boards are unaffected.
//!
//! What the check can and cannot see through the drop-in RPC, as bounds:
//!
//! - A relayed path (the miauhaus field case of 2026-11-15, a 7-hop
//!   capture via a backbone for two hours) and a vanished path are visible
//!   on every daemon. A first capture is repaired at the next beat; under a
//!   flapping path the wait doubles per recapture, so a recapture can stay
//!   off-box for up to [`PATH_SELF_REPAIR_MAX_INTERVAL_MS`] (15 min).
//! - A sibling local client holding the path at hops 0 on an abstract-socket
//!   daemon is invisible: every client renders as the same
//!   `LocalInterface[rns/<name>]`
//!   (`leviculum-std/src/interfaces/inventory.rs#local_client_name`,
//!   `LocalInterface.py:372-374`). Such a capture needs an unresponsive mark
//!   first (a request toward the owner that the owner itself let die) and
//!   ends only at the owner's next regular announce, 6 h for the Loquax home
//!   (`loquax/src/home/daemon.rs#ANNOUNCE_EVERY`), unless a relayed copy
//!   takes the path first and the beat repairs that.
//! - On a TCP shared instance each client is named by its peer port
//!   (`LocalInterface.py:439`, `incoming_connection`, at reference pin
//!   d5e62d4e15c5), and the beat compares against a baseline name learned
//!   from the first direct sight after start and after each repair
//!   announce. A sibling capture installed before the first beat after
//!   start, or between a repair announce and the next direct sight, becomes
//!   that baseline and ends only at the owner's next regular announce, as
//!   above.

/// How often the client asks its daemon about its own path. Thirty
/// seconds (the reviewer decision's starting point): the field case is
/// back within one beat.
pub(crate) const PATH_SELF_REPAIR_BEAT_MS: u64 = 30_000;

/// The floor between two repair announces of one destination: at most
/// one per minute, whatever the beat says.
pub(crate) const PATH_SELF_REPAIR_MIN_INTERVAL_MS: u64 = 60_000;

/// The ceiling the backoff doubles toward while the path keeps flipping
/// back: a flapping path costs one announce a quarter hour, not one a
/// minute, forever.
pub(crate) const PATH_SELF_REPAIR_MAX_INTERVAL_MS: u64 = 15 * 60 * 1000;

/// A repair needed again within this of the previous one counts as the
/// path flipping back, and doubles the wait; a repair after a quieter
/// stretch starts over at the floor.
pub(crate) const PATH_SELF_REPAIR_FLIP_WINDOW_MS: u64 = 10 * 60 * 1000;

/// Where the daemon says the path to the client's own destination points,
/// as the two `rnpath` RPC answers render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathSight {
    /// The daemon has no path at all (`next_hop` answered none).
    Missing,
    /// The path is relayed: `next_hop` named a transport id, not the
    /// destination itself.
    Relayed {
        /// The relay's transport id, as `next_hop` rendered it.
        via: String,
        /// The egress interface, as `next_hop_if_name` rendered it.
        if_name: String,
    },
    /// The path is direct (`next_hop` echoed the destination hash).
    Direct {
        /// The interface the direct path sits on.
        if_name: String,
    },
}

/// What one beat's observation means for one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The path is the client's own attachment as far as the RPC can see.
    Healthy,
    /// The path is wrong, but the rate limit holds the repair back.
    Wait,
    /// Announce now. `was` says what the daemon answered, for the
    /// `PATH_SELF_REPAIR` log line, as one token: `no_path`,
    /// `relayed_via_<id>_on_<interface>` or
    /// `direct_on_<interface>_not_<own>`.
    Repair { was: String },
}

/// The per-destination repair state: the baseline attachment name, the
/// last repair's time and the backed-off wait.
#[derive(Debug)]
pub(crate) struct SelfRepair {
    /// The daemon-side name of this client's own attachment, learned from
    /// the first direct sight after one of our announces; `None` until
    /// then and again right after each repair, so the next direct sight
    /// re-learns it.
    baseline_if_name: Option<String>,
    last_repair_ms: Option<u64>,
    wait_ms: u64,
    /// Whether the path has been seen healthy for a whole flip window
    /// since the last repair; the next repair then starts at the floor
    /// again instead of doubling on.
    backoff_retired: bool,
}

impl SelfRepair {
    pub(crate) fn new() -> Self {
        Self {
            baseline_if_name: None,
            last_repair_ms: None,
            wait_ms: PATH_SELF_REPAIR_MIN_INTERVAL_MS,
            backoff_retired: false,
        }
    }

    /// Judge one beat's sight of the daemon's path to our destination.
    pub(crate) fn assess(&mut self, now_ms: u64, sight: &PathSight) -> Verdict {
        let wrong = match sight {
            PathSight::Missing => Some("no_path".to_string()),
            PathSight::Relayed { via, if_name } => Some(format!("relayed_via_{via}_on_{if_name}")),
            PathSight::Direct { if_name } => match &self.baseline_if_name {
                // The first direct sight after our announce IS our own
                // attachment as far as the RPC can see: learn it.
                None => {
                    self.baseline_if_name = Some(if_name.clone());
                    None
                }
                Some(own) if own == if_name => None,
                Some(own) => Some(format!("direct_on_{if_name}_not_{own}")),
            },
        };
        let Some(was) = wrong else {
            // A healthy path quiet past the flip window retires the backoff.
            if self
                .last_repair_ms
                .is_some_and(|t| now_ms.saturating_sub(t) >= PATH_SELF_REPAIR_FLIP_WINDOW_MS)
            {
                self.wait_ms = PATH_SELF_REPAIR_MIN_INTERVAL_MS;
                self.backoff_retired = true;
            }
            return Verdict::Healthy;
        };
        match self.last_repair_ms {
            Some(t) if now_ms.saturating_sub(t) < self.wait_ms => Verdict::Wait,
            previous => {
                // The path flipped back (or stayed wrong) since a repair
                // that no quiet stretch retired: double the wait, up to
                // the ceiling. A first repair, or one after the path held
                // for a whole flip window, starts at the floor.
                self.wait_ms = if previous.is_some() && !self.backoff_retired {
                    (self.wait_ms * 2).min(PATH_SELF_REPAIR_MAX_INTERVAL_MS)
                } else {
                    PATH_SELF_REPAIR_MIN_INTERVAL_MS
                };
                self.backoff_retired = false;
                self.last_repair_ms = Some(now_ms);
                // The repair announce may install on a fresh attachment:
                // re-learn the baseline from the next direct sight.
                self.baseline_if_name = None;
                Verdict::Repair { was }
            }
        }
    }
}

/// Why one query of the beat could not ask the daemon, as the fixed code
/// the `PATH_SELF_REPAIR_SKIP` and `PATH_SELF_REPAIR_RESUMED` events carry.
/// The error's free text goes to a plain log line beside the event, never
/// into an event field: the event log splits on whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipCause {
    /// No RPC authkey could be derived (no configured `rpc_key` and no
    /// private key to derive one from).
    NoAuthkey,
    /// The daemon refused the authkey: a wrong `rpc_key`.
    AuthFailed,
    /// Nothing listens on the daemon's RPC endpoint: the daemon is down
    /// or restarting.
    Refused,
    /// Any other RPC failure: a broken connection, a timeout, an answer
    /// that did not decode.
    RpcError,
}

impl SkipCause {
    /// The single-token code for the event field.
    pub(crate) fn code(self) -> &'static str {
        match self {
            SkipCause::NoAuthkey => "no_authkey",
            SkipCause::AuthFailed => "auth_failed",
            SkipCause::Refused => "refused",
            SkipCause::RpcError => "rpc_error",
        }
    }

    /// Classify one failed RPC call.
    pub(crate) fn of_rpc(e: &crate::rpc::RpcError) -> Self {
        use crate::rpc::RpcError;
        match e {
            RpcError::AuthFailed => SkipCause::AuthFailed,
            RpcError::Io(io)
                if matches!(
                    io.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                SkipCause::Refused
            }
            _ => SkipCause::RpcError,
        }
    }
}

/// What the beat says about its own ability to ask the daemon for one
/// destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthNotice {
    /// The daemon cannot be asked about this destination, for a cause not
    /// reported for it before: warn.
    Failing { cause: SkipCause },
    /// The daemon answers again about this destination after a reported
    /// failure: say so once.
    Resumed { after: SkipCause },
}

/// Whether the beat can ask its daemon, per destination, so a beat that
/// cannot is visible at the default log level without repeating itself
/// every beat: one warning on a destination's first failure and on each
/// change of its cause, one info line when its answers come back.
///
/// Keyed per destination (audit of 1950): a healthy sibling's answer must
/// not clear a failing destination's reported cause, or that destination
/// would warn afresh on every beat.
#[derive(Debug, Default)]
pub(crate) struct BeatHealth {
    /// The cause last reported per destination; absent while answers flow.
    failing: std::collections::HashMap<leviculum_core::DestinationHash, SkipCause>,
}

impl BeatHealth {
    /// One query about `dest` failed for `cause`. Returns the warning to
    /// log, or `None` when the same cause was reported for `dest` already.
    pub(crate) fn failed(
        &mut self,
        dest: leviculum_core::DestinationHash,
        cause: SkipCause,
    ) -> Option<HealthNotice> {
        if self.failing.insert(dest, cause) == Some(cause) {
            return None;
        }
        Some(HealthNotice::Failing { cause })
    }

    /// One query about `dest` was answered. Returns the resume line to
    /// log, or `None` when nothing failed for `dest` before.
    pub(crate) fn answered(
        &mut self,
        dest: &leviculum_core::DestinationHash,
    ) -> Option<HealthNotice> {
        self.failing
            .remove(dest)
            .map(|after| HealthNotice::Resumed { after })
    }

    /// Forget the destinations the beat no longer watches.
    pub(crate) fn retain(&mut self, watched: &[leviculum_core::DestinationHash]) {
        self.failing.retain(|h, _| watched.contains(h));
    }
}

/// Build the beat's [`PathSight`] from the daemon's two `rnpath` answers,
/// decoded to JSON by the RPC client: `next_hop` is the next hop's hash as
/// lowercase hex — the destination itself on a direct path
/// (`Transport.py:1741`) — or null without a path; `next_hop_if_name` is
/// the egress interface's display name, the string `"None"` without one
/// (`leviculum-std/src/rpc/handlers.rs#NO_NEXT_HOP_INTERFACE`).
pub(crate) fn sight_from_answers(
    dest_hex: &str,
    next_hop: &serde_json::Value,
    if_name: &serde_json::Value,
) -> PathSight {
    let Some(via) = next_hop.as_str() else {
        return PathSight::Missing;
    };
    let name = match if_name.as_str() {
        Some(n) if n != "None" => n.to_string(),
        _ => return PathSight::Missing,
    };
    if via.eq_ignore_ascii_case(dest_hex) {
        PathSight::Direct { if_name: name }
    } else {
        PathSight::Relayed {
            via: via.to_string(),
            if_name: name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    fn direct() -> PathSight {
        PathSight::Direct {
            if_name: "LocalInterface[rns/test]".into(),
        }
    }

    fn relayed() -> PathSight {
        PathSight::Relayed {
            via: "6f5a1b2c".into(),
            if_name: "TCPInterface[backbone/10.0.0.1:4242]".into(),
        }
    }

    /// The healthy steady state: a direct path on the learned attachment
    /// never repairs.
    #[test]
    fn a_direct_path_on_the_own_attachment_is_healthy() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        assert_eq!(s.assess(T0 + 30_000, &direct()), Verdict::Healthy);
    }

    /// The field case: the path went relayed; the next beat repairs.
    #[test]
    fn a_relayed_path_repairs_at_the_next_beat() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        match s.assess(T0 + 30_000, &relayed()) {
            Verdict::Repair { was } => {
                assert!(was.contains("6f5a1b2c"), "the log names the relay: {was}");
            }
            other => panic!("a relayed path must repair, got {other:?}"),
        }
    }

    /// A vanished path (dropped over RPC, daemon restart) repairs too.
    #[test]
    fn a_missing_path_repairs() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        assert!(matches!(
            s.assess(T0 + 30_000, &PathSight::Missing),
            Verdict::Repair { .. }
        ));
    }

    /// Test (c) of the reviewer decision, at the decision level: a sibling
    /// client's capture shows as a direct path on another attachment name,
    /// and does not survive the next beat.
    #[test]
    fn a_direct_path_on_another_attachment_repairs() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        let sibling = PathSight::Direct {
            if_name: "LocalInterface[37429]".into(),
        };
        assert!(
            matches!(s.assess(T0 + 30_000, &sibling), Verdict::Repair { .. }),
            "a replaying sibling cannot keep the path past one beat"
        );
    }

    /// The first sight learns the baseline rather than repairing: before
    /// our announce installed, there is nothing to compare against.
    #[test]
    fn the_first_direct_sight_learns_the_baseline() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        // The learned name now IS the baseline: a different one repairs.
        assert!(matches!(
            s.assess(
                T0 + 30_000,
                &PathSight::Direct {
                    if_name: "LocalInterface[9999]".into()
                }
            ),
            Verdict::Repair { .. }
        ));
    }

    /// Test (d) of the reviewer decision: the rate limit holds under a
    /// flapping path — at most one repair per wait, and the wait doubles
    /// while the path keeps flipping back, up to the ceiling.
    #[test]
    fn the_rate_limit_holds_under_a_flapping_path() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);

        // First capture: repairs at once.
        assert!(matches!(
            s.assess(T0 + 30_000, &relayed()),
            Verdict::Repair { .. }
        ));

        // Still wrong one beat later: inside the minute, held back.
        assert_eq!(s.assess(T0 + 60_000, &relayed()), Verdict::Wait);

        // Wrong again past the minute: repairs, and the wait doubles.
        assert!(matches!(
            s.assess(T0 + 30_000 + 61_000, &relayed()),
            Verdict::Repair { .. }
        ));
        // The doubled wait (2 min) holds a repair at +90 s back.
        assert_eq!(
            s.assess(T0 + 30_000 + 61_000 + 90_000, &relayed()),
            Verdict::Wait
        );
        // And releases it after the full two minutes.
        assert!(matches!(
            s.assess(T0 + 30_000 + 61_000 + 121_000, &relayed()),
            Verdict::Repair { .. }
        ));
    }

    /// The backoff never grows past the ceiling, however long the flap.
    #[test]
    fn the_backoff_is_capped() {
        let mut s = SelfRepair::new();
        let mut now = T0;
        let mut repairs = 0;
        // Observe a permanently wrong path every 30 s for three hours.
        while now < T0 + 3 * 60 * 60 * 1000 {
            if matches!(s.assess(now, &relayed()), Verdict::Repair { .. }) {
                repairs += 1;
            }
            now += 30_000;
        }
        // Floor: at the 15-minute ceiling three hours allow ~12 repairs;
        // without a cap the doubling would have stopped after ~8 total.
        assert!(
            (10..=20).contains(&repairs),
            "three flapping hours must settle at the ceiling: {repairs} repairs"
        );
    }

    /// A repair after a quiet stretch starts over at the one-minute floor
    /// rather than inheriting an old backoff.
    #[test]
    fn a_quiet_stretch_resets_the_backoff() {
        let mut s = SelfRepair::new();
        assert!(matches!(s.assess(T0, &relayed()), Verdict::Repair { .. }));
        assert!(matches!(
            s.assess(T0 + 61_000, &relayed()),
            Verdict::Repair { .. }
        ));
        // Healed, and quiet past the flip window.
        let quiet_end = T0 + 61_000 + PATH_SELF_REPAIR_FLIP_WINDOW_MS + 1_000;
        assert_eq!(s.assess(quiet_end, &direct()), Verdict::Healthy);
        // A fresh capture repairs at once, and a follow-up only a little
        // past one minute repairs too: the wait is back at the floor.
        assert!(matches!(
            s.assess(quiet_end + 30_000, &relayed()),
            Verdict::Repair { .. }
        ));
        assert!(matches!(
            s.assess(quiet_end + 30_000 + 61_000, &relayed()),
            Verdict::Repair { .. }
        ));
    }

    fn hash(b: u8) -> leviculum_core::DestinationHash {
        leviculum_core::DestinationHash::new([b; 16])
    }

    /// Audit of 1939: a beat that cannot ask its daemon warns on the first
    /// failure, stays quiet while the cause repeats, warns again when the
    /// cause changes, and says once at info that answers resumed.
    #[test]
    fn a_failing_beat_warns_once_per_cause_and_once_on_resume() {
        let a = hash(1);
        let mut h = BeatHealth::default();
        assert_eq!(h.answered(&a), None, "nothing to resume before any failure");
        assert_eq!(
            h.failed(a, SkipCause::AuthFailed),
            Some(HealthNotice::Failing {
                cause: SkipCause::AuthFailed
            })
        );
        assert_eq!(
            h.failed(a, SkipCause::AuthFailed),
            None,
            "no repeat per beat"
        );
        assert_eq!(
            h.failed(a, SkipCause::AuthFailed),
            None,
            "no repeat per beat"
        );
        assert_eq!(
            h.failed(a, SkipCause::Refused),
            Some(HealthNotice::Failing {
                cause: SkipCause::Refused
            }),
            "a changed cause is reported"
        );
        assert_eq!(
            h.answered(&a),
            Some(HealthNotice::Resumed {
                after: SkipCause::Refused
            })
        );
        assert_eq!(h.answered(&a), None, "the resume is said once");
        assert_eq!(
            h.failed(a, SkipCause::AuthFailed),
            Some(HealthNotice::Failing {
                cause: SkipCause::AuthFailed
            }),
            "a failure after a resume is reported afresh"
        );
    }

    /// Audit of 1950: a beat watching two destinations, one failing with a
    /// constant cause and one healthy, warns exactly once over ten beats.
    /// The healthy sibling's answers do not clear the failing one's cause.
    #[test]
    fn a_healthy_sibling_does_not_clear_a_failing_destinations_cause() {
        let (failing, healthy) = (hash(1), hash(2));
        let mut h = BeatHealth::default();
        let mut warns = 0;
        let mut resumes = 0;
        for _beat in 0..10 {
            for (dest, ok) in [(failing, false), (healthy, true)] {
                let notice = if ok {
                    h.answered(&dest)
                } else {
                    h.failed(dest, SkipCause::RpcError)
                };
                match notice {
                    Some(HealthNotice::Failing { .. }) => warns += 1,
                    Some(HealthNotice::Resumed { .. }) => resumes += 1,
                    None => {}
                }
            }
        }
        assert_eq!(warns, 1, "one warn for the cause, not one per beat");
        assert_eq!(resumes, 0, "the failing destination never resumed");
    }

    /// A destination the beat stops watching is forgotten: if it comes
    /// back and fails, that is reported afresh.
    #[test]
    fn a_destination_no_longer_watched_is_forgotten() {
        let (a, b) = (hash(1), hash(2));
        let mut h = BeatHealth::default();
        assert!(h.failed(a, SkipCause::Refused).is_some());
        assert!(h.failed(b, SkipCause::Refused).is_some());
        h.retain(&[b]);
        assert!(
            h.failed(b, SkipCause::Refused).is_none(),
            "b is still known"
        );
        assert!(
            h.failed(a, SkipCause::Refused).is_some(),
            "a was forgotten and is reported afresh"
        );
    }

    /// Every cause code is one token the event log takes as it is.
    #[test]
    fn skip_cause_codes_are_single_tokens() {
        let codes = [
            SkipCause::NoAuthkey,
            SkipCause::AuthFailed,
            SkipCause::Refused,
            SkipCause::RpcError,
        ]
        .map(SkipCause::code);
        assert_eq!(codes, ["no_authkey", "auth_failed", "refused", "rpc_error"]);
    }

    /// The RPC errors map onto their codes: a refused authkey, a missing
    /// listener, and everything else.
    #[test]
    fn rpc_errors_classify_by_kind() {
        use crate::rpc::RpcError;
        use std::io::{Error, ErrorKind};
        assert_eq!(
            SkipCause::of_rpc(&RpcError::AuthFailed),
            SkipCause::AuthFailed
        );
        assert_eq!(
            SkipCause::of_rpc(&RpcError::Io(Error::from(ErrorKind::ConnectionRefused))),
            SkipCause::Refused
        );
        assert_eq!(
            SkipCause::of_rpc(&RpcError::Io(Error::from(ErrorKind::NotFound))),
            SkipCause::Refused
        );
        assert_eq!(
            SkipCause::of_rpc(&RpcError::Io(Error::from(ErrorKind::TimedOut))),
            SkipCause::RpcError
        );
        assert_eq!(
            SkipCause::of_rpc(&RpcError::Pickle("bad".into())),
            SkipCause::RpcError
        );
    }

    /// Every `was` the beat logs is one token (audit of 1950).
    #[test]
    fn repair_reasons_are_single_tokens() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        let Verdict::Repair { was } = s.assess(T0 + 30_000, &relayed()) else {
            panic!("a relayed path repairs");
        };
        assert_eq!(
            was,
            "relayed_via_6f5a1b2c_on_TCPInterface[backbone/10.0.0.1:4242]"
        );
        assert_eq!(s.assess(T0 + 60_000, &direct()), Verdict::Healthy);
        let Verdict::Repair { was } = s.assess(
            T0 + 200_000,
            &PathSight::Direct {
                if_name: "LocalInterface[9999]".into(),
            },
        ) else {
            panic!("a direct path on another attachment repairs");
        };
        assert_eq!(
            was,
            "direct_on_LocalInterface[9999]_not_LocalInterface[rns/test]"
        );
        let mut s = SelfRepair::new();
        let Verdict::Repair { was } = s.assess(T0, &PathSight::Missing) else {
            panic!("a missing path repairs");
        };
        assert_eq!(was, "no_path");
    }

    /// The two RPC answers of a healthy client read as a direct sight,
    /// a transport id as relayed, a null as missing.
    #[test]
    fn sights_follow_the_rnpath_answers() {
        let dest = "00112233445566778899aabbccddeeff";
        let name = serde_json::json!("LocalInterface[rns/test]");
        assert_eq!(
            sight_from_answers(dest, &serde_json::json!(dest), &name),
            PathSight::Direct {
                if_name: "LocalInterface[rns/test]".into()
            }
        );
        assert_eq!(
            sight_from_answers(
                dest,
                &serde_json::json!("ffeeddccbbaa99887766554433221100"),
                &name
            ),
            PathSight::Relayed {
                via: "ffeeddccbbaa99887766554433221100".into(),
                if_name: "LocalInterface[rns/test]".into()
            }
        );
        assert_eq!(
            sight_from_answers(dest, &serde_json::Value::Null, &name),
            PathSight::Missing
        );
        // Python renders "no interface" as the string "None"
        // (Reticulum.py:1597-1605): that is no path, not an interface.
        assert_eq!(
            sight_from_answers(dest, &serde_json::json!(dest), &serde_json::json!("None")),
            PathSight::Missing
        );
    }

    /// After a repair the baseline is re-learned from the next direct
    /// sight: the repair announce may install on a fresh attachment (a
    /// reconnect), and the old name must not shadow it.
    #[test]
    fn a_repair_relearns_the_baseline() {
        let mut s = SelfRepair::new();
        assert_eq!(s.assess(T0, &direct()), Verdict::Healthy);
        assert!(matches!(
            s.assess(T0 + 30_000, &relayed()),
            Verdict::Repair { .. }
        ));
        // Our announce installed on a NEW attachment name (reconnect):
        // that is the repaired state, not a sibling.
        let fresh = PathSight::Direct {
            if_name: "LocalInterface[rns/test-reborn]".into(),
        };
        assert_eq!(s.assess(T0 + 60_000, &fresh), Verdict::Healthy);
        assert_eq!(s.assess(T0 + 90_000, &fresh), Verdict::Healthy);
    }
}
