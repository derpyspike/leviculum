# Leviculum patch to trouble-host 0.8.0

The registry sources of `trouble-host` 0.8.0 (Apache-2.0 OR MIT,
embassy-rs) plus connection-specific completed-packet callbacks. The
registry host reports completions without saying whose they are, and it
keeps `command()` crate-private, so the feature cannot be added from
outside. `leviculum-trouble` builds this copy by path; nothing in this
tree takes upstream TrouBLE from git. The package version carries
`+leviculum` build metadata because Cargo refuses two packages of one
name and version in a lockfile. The patch is offered upstream once a
second controller has proved it on a bench (the ESP32-S3 runs it; the
CYW43439 has not, see `docs/ble5-broadcast-cyw43439-spike.md`).
`rustfmt.toml` disables formatting here: the workspaces' `cargo fmt
--all` reaches this path dependency, and the vendored sources keep
their own layout. The crate allows upstream's clippy and rustdoc styles
(`src/lib.rs`, the four `allow` lines at the top) for the same reason.

## What the patch adds

`EventHandler::on_packets_completed` receives the actual `ConnHandle`
and count from each HCI completed-packets entry. This is a clean API
cutover: adapters route nonzero completions to that connection's drain
slot, never every link. A raw-H4 regression exercises the actual RX
runner and callback dispatch (`src/host.rs`, unit tests).

A periodic-advertising extension (trains, syncs, a sync table) rode in
this copy from its first vendoring until order 1715 removed it: nothing
in this tree enabled its feature, and 991 lines of dead vendored code
are a liability, not a reserve. The BT5-broadcast roadmap re-adds it
from the fork's history the day a controller is ready to prove it.

## Files dropped from the registry package

`Cargo.toml.orig`, `Cargo.lock`, `.cargo-ok`, `.cargo_vcs_info.json`.
The upstream `tests/` directory and its `[[test]]` declarations are not
carried: those suites drive two real HCI controllers
(`common::find_controllers`) and can never run on this tree's hosts or
its forge, while the integ census (`scripts/check-standard-integ.py`)
rightly refuses test targets nothing runs. The patch's own regression
lives in the library and runs as a unit test — the crate is a
workspace member for exactly that.
