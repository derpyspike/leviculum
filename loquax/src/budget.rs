//! The map's memory and frame budgets from the machine it runs on (Lew's
//! rule 6 of order 1836, built in order 1910): one function,
//! [`budgets`], takes the facts of the machine ([`Machine`]) and says how
//! many bytes the tile cache holds, how many missing tiles a moving frame
//! draws, and how many tiles a view at rest prefetches. Before, the three
//! were constants (96 MB, two tiles a worker, 128 tiles), the same on a
//! 1 GB phone and a 64 GB desktop.
//!
//! - The tile cache is a fortieth of the memory, never below
//!   [`CACHE_FLOOR_MB`] (a phone's frame with its margins, about 55 tiles
//!   of 256 KiB, twice) nor above [`CACHE_CEILING_MB`], the 96 MB the
//!   render walk's heap bounds were measured with: a 1 GB phone keeps a
//!   third of that, a 4 GB phone and anything larger all of it.
//! - A moving frame draws [`TILES_PER_CORE`] missing tiles a core, never
//!   more than [`FRAME_TILES_MAX`]: the rest stand in from the step
//!   before and are drawn in the frames that follow.
//! - A view at rest prefetches at most a third of what the cache holds,
//!   never more than [`PREFETCH_MAX`]: the frame's own tiles keep the
//!   rest.
//!
//! The facts are read once at start ([`Machine::here`]) and the result
//! is said in one structured event, `MAP_BUDGET`.

/// A tile's bytes in the cache: 256 by 256 pixels of RGBA.
const TILE_BYTES: u64 = 256 * 256 * 4;
/// The least tile cache, in MiB.
pub const CACHE_FLOOR_MB: u64 = 32;
/// The most tile cache, in MiB.
pub const CACHE_CEILING_MB: u64 = 96;
/// The share of memory the tile cache takes: one part in this many.
pub const CACHE_SHARE: u64 = 40;
/// Missing tiles a moving frame draws a core.
pub const TILES_PER_CORE: usize = 2;
/// Missing tiles a moving frame draws at most.
pub const FRAME_TILES_MAX: usize = 16;
/// Tiles a view at rest prefetches at most.
pub const PREFETCH_MAX: usize = 128;
/// The memory assumed where none can be read: a small phone's.
pub const RAM_UNKNOWN_MB: u64 = 1024;

/// What the budgets are made of: the machine's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// Its memory, in MiB.
    pub ram_mb: u64,
    /// Its cores.
    pub cores: usize,
}

/// The map's budgets on a machine ([`budgets`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// Bytes the tile cache holds.
    pub tile_cache_bytes: usize,
    /// Missing tiles a moving frame draws.
    pub frame_tiles: usize,
    /// Tiles a view at rest prefetches at most.
    pub prefetch_tiles: usize,
}

/// The budgets of `machine`: the one rule (rule 6).
pub fn budgets(machine: &Machine) -> Budgets {
    let cache_mb = (machine.ram_mb / CACHE_SHARE).clamp(CACHE_FLOOR_MB, CACHE_CEILING_MB);
    let cache_bytes = cache_mb << 20;
    let cache_tiles = usize::try_from(cache_bytes / TILE_BYTES).unwrap_or(usize::MAX);
    Budgets {
        tile_cache_bytes: usize::try_from(cache_bytes).unwrap_or(usize::MAX),
        frame_tiles: machine
            .cores
            .max(1)
            .saturating_mul(TILES_PER_CORE)
            .min(FRAME_TILES_MAX),
        prefetch_tiles: (cache_tiles / 3).min(PREFETCH_MAX),
    }
}

impl Machine {
    /// The machine this runs on: its memory from `/proc/meminfo` where
    /// there is one (Linux, Android), else [`RAM_UNKNOWN_MB`]; its cores
    /// as the painters count them.
    pub fn here() -> Self {
        let ram_mb = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| mem_total_mb(&m))
            .unwrap_or(RAM_UNKNOWN_MB);
        Self {
            ram_mb,
            cores: crate::picture::cores(),
        }
    }
}

/// `MemTotal` of a `/proc/meminfo` text, in MiB.
fn mem_total_mb(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// The budgets of the machine this runs on, said once in the structured
/// event `MAP_BUDGET` the first time they are asked.
pub fn here() -> Budgets {
    static HERE: std::sync::OnceLock<Budgets> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| {
        let machine = Machine::here();
        let b = budgets(&machine);
        said(&machine, &b);
        b
    })
}

/// The start event: the facts and what they gave.
fn said(machine: &Machine, b: &Budgets) {
    tracing::info!(
        event = "MAP_BUDGET",
        ram_mb = machine.ram_mb,
        cores = machine.cores,
        tile_cache_mb = b.tile_cache_bytes >> 20,
        frame_tiles = b.frame_tiles,
        prefetch_tiles = b.prefetch_tiles,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 GB phone: the cache at its floor, 32 MB, two cores' tiles a
    /// moving frame, a third of the cache's 128 tiles prefetched.
    #[test]
    fn a_1_gb_phone_keeps_the_floor() {
        let b = budgets(&Machine {
            ram_mb: 1024,
            cores: 2,
        });
        assert_eq!(
            b,
            Budgets {
                tile_cache_bytes: 32 << 20,
                frame_tiles: 4,
                prefetch_tiles: 42,
            }
        );
    }

    /// A 4 GB phone with eight cores: a fortieth (102 MB) held to the
    /// ceiling, what the constant gave (96 MB); sixteen tiles a moving
    /// frame, the full prefetch.
    #[test]
    fn a_4_gb_phone_keeps_about_the_old_cache() {
        let b = budgets(&Machine {
            ram_mb: 4096,
            cores: 8,
        });
        assert_eq!(
            b,
            Budgets {
                tile_cache_bytes: 96 << 20,
                frame_tiles: 16,
                prefetch_tiles: 128,
            }
        );
    }

    /// A 64 GB desktop with 32 cores: the cache at its ceiling, 96 MB,
    /// and the frame's tiles at their most, not a fortieth of 64 GB.
    #[test]
    fn a_64_gb_desktop_stops_at_the_ceiling() {
        let b = budgets(&Machine {
            ram_mb: 65536,
            cores: 32,
        });
        assert_eq!(
            b,
            Budgets {
                tile_cache_bytes: 96 << 20,
                frame_tiles: 16,
                prefetch_tiles: 128,
            }
        );
    }

    /// The memory read from `/proc/meminfo`, and none from text without
    /// it.
    #[test]
    fn the_memory_is_read_from_meminfo() {
        let text = "MemTotal:       16314200 kB\nMemFree:         1234 kB\n";
        assert_eq!(mem_total_mb(text), Some(15931));
        assert_eq!(mem_total_mb("MemFree: 1 kB\n"), None);
        assert_eq!(mem_total_mb("MemTotal: x kB\n"), None);
    }

    /// The start event carries every key of its catalogue entry, read by
    /// a subscriber of the test's own.
    #[test]
    fn the_start_event_says_the_facts_and_the_budgets() {
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing::span::{Attributes, Id, Record};
        use tracing::{Event, Metadata};
        #[derive(Default)]
        struct Seen(Arc<Mutex<Vec<String>>>);
        struct Fields<'a>(&'a mut Vec<String>);
        impl Visit for Fields<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
                self.0.push(format!("{}={value:?}", field.name()));
            }
        }
        impl tracing::Subscriber for Seen {
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &Attributes<'_>) -> Id {
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, _: &Record<'_>) {}
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, event: &Event<'_>) {
                let mut seen = self.0.lock().expect("seen");
                event.record(&mut Fields(&mut seen));
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let machine = Machine {
            ram_mb: 4096,
            cores: 8,
        };
        tracing::subscriber::with_default(Seen(Arc::clone(&seen)), || {
            said(&machine, &budgets(&machine))
        });
        let fields = seen.lock().expect("seen").clone();
        assert_eq!(
            fields,
            [
                "event=\"MAP_BUDGET\"",
                "ram_mb=4096",
                "cores=8",
                "tile_cache_mb=96",
                "frame_tiles=16",
                "prefetch_tiles=128",
            ]
        );
    }
}
