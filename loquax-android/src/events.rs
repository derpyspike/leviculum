//! The first start's structured record (1798, render walk 2026-11-13
//! finding 2): the fresh install that chose "on this phone" failed after
//! the whole join budget with not one line in logcat, so nobody could say
//! which step died. Each step of [`crate::session::Session::start`] says
//! one `FIRST_START` line, and the mailbox running inside the app says
//! `MAILBOX_HERE` when it comes up and when it ends — its `Err` reached
//! nothing before. The lines go to logcat through the subscriber
//! `crate::logcat::install` (an Android-only module, hence no link)
//! registers, which carries every target under `loquax`.

use leviculum_core::event_scalar::Scalar;

/// A step of the first start, `ok` whether it went through, `detail` the
/// step's own word (an error's words, a chosen server, `-` for none).
pub fn first_start(step: &str, ok: bool, detail: &str) {
    let detail = match detail {
        "" => "-".to_string(),
        words => Scalar(words).to_string(),
    };
    tracing::info!(
        event = "FIRST_START",
        step = %Scalar(step),
        ok = ok,
        detail = %detail,
    );
}

/// The mailbox on this phone changed state: `spawned` when its task
/// starts, `ended` with the daemon's own words when it ends.
pub fn mailbox_here(state: &str, detail: &str) {
    let detail = match detail {
        "" => "-".to_string(),
        words => Scalar(words).to_string(),
    };
    tracing::info!(
        event = "MAILBOX_HERE",
        state = %Scalar(state),
        detail = %detail,
    );
}
