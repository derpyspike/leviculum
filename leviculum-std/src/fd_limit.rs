//! The process's file-descriptor allowance, raised once at start.
//!
//! Shells and app launchers hand a process a soft `RLIMIT_NOFILE` of 1024
//! under a hard limit orders of magnitude higher. A node or a messenger
//! holds descriptors wide — sockets, the message store, the map's region
//! files — and treating the shell's default as a budget starved the
//! Loquax window of descriptors on 2026-11-16 (order 1942 item 3: 71
//! region files held open 14 times filled the 1024). Raising the soft
//! limit to the hard one at start is the belt every server wears; the
//! fix for any one consumer is still to hold fewer descriptors.
//!
//! The TCP interface raises the limit for the daemon when it starts
//! listening; this is the same raise for a binary's own `main`, so a
//! frontend that never listens is covered too.

/// Raise the soft `RLIMIT_NOFILE` to the hard limit, once per process,
/// logged once as `FD_LIMIT soft=… hard=… raised_from=…`. The limits
/// after the raise as `(soft, hard, raised_from)`; `None` where they
/// cannot be read, and on targets other than Linux and Android, which
/// are left alone.
pub fn raise_to_hard() -> Option<(u64, u64, u64)> {
    crate::interfaces::tcp_admission::raise_nofile_limit().map(|l| (l.soft, l.hard, l.raised_from))
}
