# Leviculum patch to trouble-host 0.8.0

The registry sources of `trouble-host` 0.8.0 (Apache-2.0 OR MIT, embassy-rs)
plus periodic advertising trains/sync and connection-specific completed-packet
callbacks. The registry host drops periodic LE events and keeps `command()`
crate-private, so those features cannot be added from outside.
`leviculum-rp` builds it only under its `trouble-vendored` feature, as the
renamed dependency `trouble-host-vendored`; the default build takes upstream
TrouBLE (`https://github.com/embassy-rs/trouble`, a pinned revision) as a git
dependency. The package version carries
`+leviculum` build metadata because Cargo refuses two packages of one
name and version in a lockfile. The patch is offered upstream once a
controller with the feature has proved it on a bench (the CYW43439 has not,
see `docs/ble5-broadcast-cyw43439-spike.md`). `rustfmt.toml` disables
formatting here: the workspaces' `cargo fmt --all` reaches this path
dependency, and the vendored sources keep their own layout.

## What the patch adds

`EventHandler::on_packets_completed` receives the actual `ConnHandle` and count
from each HCI completed-packets entry. This is a clean API cutover: adapters
route nonzero completions to that connection's drain slot, never every link.
A raw-H4 regression exercises the actual RX runner and callback dispatch.

New feature `periodic-advertising` (implies `extended-advertising`) and the
config knob `PERIODIC_SYNCS_MAX` (default 4; `periodic-syncs-max-{1,2,4,8}`
or `TROUBLE_HOST_PERIODIC_SYNCS_MAX`, the crate's own mechanism).

- `src/periodic.rs` (new):
  - `Peripheral::periodic_advertise(adv_handle, &PeriodicAdvParams, data)`
    runs a train on an extended advertising set (`LE Set Periodic
    Advertising Parameters`, `… Data` in fragments, `… Enable`) and returns
    a `PeriodicAdvertiser` with `update(data)` and `stop()`; dropping it
    asks the runner to disable the train.
  - `Central::periodic_sync(&PeriodicSyncConfig)` creates a sync
    (`LE Periodic Advertising Create Sync`), waits for `LE Periodic
    Advertising Sync Established` up to `create_timeout` (then cancels and
    returns `Error::Timeout`) and returns a `PeriodicSync` with
    `next_report()` (one reassembled `PeriodicReport` per periodic event,
    `Error::PeriodicSyncLost` after `LE Periodic Advertising Sync Lost`)
    and `terminate()`; dropping it asks the runner to terminate the sync,
    dropping the create future asks it to cancel the create.
  - `PeriodicState`, the fixed sync table in the host state: one pending
    create at a time, a train held once, `PERIODIC_SYNCS_MAX` slots,
    reports reassembled from the controller's `data_status` chunks into
    `PERIODIC_REPORT_MAX` (255) bytes with truncation flagged, counters.
    Unit-tested in the module (`cargo test --lib --features
    periodic-advertising,scan,peripheral,central,gatt,derive,default-packet-pool,connection-params-update`).
- `src/host.rs`: the sync table in `HostState`, the three LE event mask
  bits, the three event arms, a `CancelledCommandState::Periodic` action
  the runner services (cancel create, terminate sync, disable train), and
  three command bounds on the runner (`LePeriodicAdvCreateSyncCancel`,
  `LePeriodicAdvTerminateSync`, `LeSetPeriodicAdvEnable`; every
  `ExternalController` has them).
- `src/lib.rs`: `pub mod periodic`, its prelude export,
  `Error::PeriodicSyncLost`.
- `src/peripheral.rs`: `Peripheral::host` is `pub(crate)`.
- `Cargo.toml`, `build.rs`, `src/config.rs`: the feature and the knob.

## What it does not do

Periodic advertising sync transfer (PAST), periodic advertising with
responses (PAwR: subevents, response slots), constant tone extensions,
`LE Set Periodic Advertising Receive Enable`, the periodic advertiser
list. A `PeriodicAdvertiser` or `PeriodicSync` dropped while the runner
is not running leaves its request queued until it runs.

## Files dropped from the registry package

`Cargo.toml.orig`, `Cargo.lock`, `.cargo-ok`, `.cargo_vcs_info.json`. The
integration tests under `tests/` need two Zephyr HCI UART adapters and are
not part of any recipe here.
