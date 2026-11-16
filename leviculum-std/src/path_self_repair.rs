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
//! What the check can and cannot see through the drop-in RPC: a relayed
//! path (the miauhaus field case of 2026-11-15, a 7-hop capture via a
//! backbone for two hours) and a vanished path are visible on every
//! daemon. A sibling local client holding the path at hops 0 is visible
//! only when the daemon names client attachments apart: on a TCP shared
//! instance each client is named by its peer port
//! (`LocalInterface.py:433`), while on an abstract-socket daemon every
//! client renders as the same `LocalInterface[rns/<name>]`
//! (`leviculum-std/src/interfaces/inventory.rs#local_client_name`,
//! `LocalInterface.py:372-374`), so there the sibling case is caught only
//! once the path leaves the local attachment. Such a capture needs an
//! unresponsive mark first — a request toward the owner that the owner
//! itself let die — and ends with the owner's next announce.

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
    /// `PATH_SELF_REPAIR` log line.
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
            PathSight::Missing => Some("no path".to_string()),
            PathSight::Relayed { via, if_name } => Some(format!("relayed via {via} on {if_name}")),
            PathSight::Direct { if_name } => match &self.baseline_if_name {
                // The first direct sight after our announce IS our own
                // attachment as far as the RPC can see: learn it.
                None => {
                    self.baseline_if_name = Some(if_name.clone());
                    None
                }
                Some(own) if own == if_name => None,
                Some(own) => Some(format!("hops 0 on {if_name}, not {own}")),
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

/// What the beat says about its own ability to ask the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthNotice {
    /// The daemon cannot be asked, for a cause not reported before: warn.
    Failing { reason: String },
    /// The daemon answers again after a reported failure: say so once.
    Resumed { after: String },
}

/// Whether the beat can ask its daemon, so a beat that cannot is visible at
/// the default log level without repeating itself every beat: one warning
/// on the first failure and on each change of cause, one info line when the
/// answers come back.
#[derive(Debug, Default)]
pub(crate) struct BeatHealth {
    /// The cause of the failure last reported, `None` while answers flow.
    failing: Option<String>,
}

impl BeatHealth {
    /// One query failed for `reason`. Returns the warning to log, or `None`
    /// when the same cause was reported already.
    pub(crate) fn failed(&mut self, reason: &str) -> Option<HealthNotice> {
        if self.failing.as_deref() == Some(reason) {
            return None;
        }
        self.failing = Some(reason.to_string());
        Some(HealthNotice::Failing {
            reason: reason.to_string(),
        })
    }

    /// One query was answered. Returns the resume line to log, or `None`
    /// when nothing failed before.
    pub(crate) fn answered(&mut self) -> Option<HealthNotice> {
        self.failing
            .take()
            .map(|after| HealthNotice::Resumed { after })
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

    /// Audit of 1939: a beat that cannot ask its daemon warns on the first
    /// failure, stays quiet while the cause repeats, warns again when the
    /// cause changes, and says once at info that answers resumed.
    #[test]
    fn a_failing_beat_warns_once_per_cause_and_once_on_resume() {
        let mut h = BeatHealth::default();
        assert_eq!(h.answered(), None, "nothing to resume before any failure");
        assert_eq!(
            h.failed("RPC failed: auth"),
            Some(HealthNotice::Failing {
                reason: "RPC failed: auth".into()
            })
        );
        assert_eq!(h.failed("RPC failed: auth"), None, "no repeat per beat");
        assert_eq!(h.failed("RPC failed: auth"), None, "no repeat per beat");
        assert_eq!(
            h.failed("RPC failed: refused"),
            Some(HealthNotice::Failing {
                reason: "RPC failed: refused".into()
            }),
            "a changed cause is reported"
        );
        assert_eq!(
            h.answered(),
            Some(HealthNotice::Resumed {
                after: "RPC failed: refused".into()
            })
        );
        assert_eq!(h.answered(), None, "the resume is said once");
        assert_eq!(
            h.failed("RPC failed: auth"),
            Some(HealthNotice::Failing {
                reason: "RPC failed: auth".into()
            }),
            "a failure after a resume is reported afresh"
        );
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
