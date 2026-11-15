//! The one seam this crate has upward into its embedder: where a log
//! line goes.
//!
//! The nRF family rings its lines through `leviculum-firmware`'s log
//! module (DTR-gated, drained by the USB task); the M9 rings them
//! through `leviculum-trouble`'s (drained onto UART0 between frames).
//! This crate cannot name either, so the embedder installs a sink once
//! at boot and every line here goes through it. No sink, no line: the
//! install points run before any BLE task exists, so nothing is lost on
//! a real boot, and the crate's own tests assert behaviour, not lines.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

/// Whether a line must survive a closed runtime drain (the nRF's DTR
/// gate holds runtime lines back until a host listens; boot-critical
/// lines bypass it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// An operational line: gated however the embedder gates them.
    Runtime,
    /// A line that must reach the boot scrollback.
    Critical,
}

/// What the embedder installs: prefix, formatted body, level.
pub type Sink = fn(&str, core::fmt::Arguments<'_>, Level);

static SINK: Mutex<CriticalSectionRawMutex, Cell<Option<Sink>>> = Mutex::new(Cell::new(None));

/// Install the embedder's sink. The last install wins; both families
/// install exactly one, before any BLE task runs. A second install with
/// the same sink is a no-op by construction, so the nRF's choke point
/// (`channels()`) may run on every boot path without a guard.
pub fn set_sink(sink: Sink) {
    SINK.lock(|cell| cell.set(Some(sink)));
}

pub(crate) fn log_fmt(prefix: &str, args: core::fmt::Arguments<'_>) {
    if let Some(sink) = SINK.lock(|cell| cell.get()) {
        sink(prefix, args, Level::Runtime);
    }
}

/// For a line that must reach the boot scrollback. No line this crate
/// writes takes this route today; the shared line shapes
/// (`leviculum_log_line::facts`) carry their route as data, and the
/// crate's `LineSink` maps the critical one here.
pub(crate) fn log_fmt_critical(prefix: &str, args: core::fmt::Arguments<'_>) {
    if let Some(sink) = SINK.lock(|cell| cell.get()) {
        sink(prefix, args, Level::Critical);
    }
}
