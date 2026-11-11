//! Columba v2.2 over the TrouBLE host, generic over the HCI transport.
//!
//! The protocol half of the fork's BLE backend (`cedrivard/Leviculum`,
//! `leviculum-fw/src/ble`, adopted from `origin/m9-serial-robustness` at
//! 6a647b760), rehomed as a crate of its own: the nRF boards speak Columba
//! through the SoftDevice (`leviculum-nrf/src/ble`), this crate speaks it
//! through TrouBLE over anything that answers HCI — esp-radio's
//! `BleConnector` on the ESP32-S3 today, the CYW43439's `BtDriver` in the
//! fork. The decisions themselves stay in `leviculum-ble-tx`, host-tested;
//! this crate performs them, exactly as the nRF backend does.
//!
//! What the embedding firmware provides and consumes:
//!
//! - it hands [`name::note_boot_identity`] the identity hash, builds the
//!   stack with [`ble::build_stack`], registers the GATT service with
//!   [`ble::columba::start`], and drives [`ble::run_columba`] — as an
//!   executor task or polled from a loop; every future here tolerates a
//!   no-op waker, it only costs latency,
//! - it drains [`log::LOG_RING`] to its debug surface,
//! - it reads packets and peer transitions from [`ble::channels`] and
//!   feeds its node; outbound it sends through the [`ble::BleInterface`]
//!   it registered with the node.
//!
//! No executor, no chip, no scheduler is named anywhere in this crate:
//! that is the embedding firmware's half, and the reason the M9's
//! loop-polled image and the fork's embassy images share this code
//! unchanged.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod ble;
pub mod iface_bytes;
pub mod log;
pub mod media;
pub mod name;
