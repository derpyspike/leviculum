//! A box of an archive, local or remote, cut into a region under a size
//! cap (1089): the tiles are located first, from the directories alone, and
//! when the box's tiles down to the deepest zoom asked would fetch more than
//! the cap, the deepest zoom is lowered until they do not ([`plan`]). Only
//! then is anything fetched, a round of the cut at a time, coarse to fine,
//! each round in as few ranges as the archive's layout allows, so the region
//! file shows its shallow zooms while the deep ones still arrive (1291,
//! [`cut_planned`]). A load lives across calls ([`Loading`], 1836): it can
//! stop between two rounds and go on later from where it was.

use std::collections::HashMap;
use std::io::{Seek, Write};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{PoisonError, RwLock};

use leviculum_map::region::{Bounds, TileId};

#[cfg(doc)]
use crate::cut::Progress;
use crate::cut::{in_bounds, CutOptions, CutStats, Streaming};
use crate::pmtiles::{Archive, Fetched, Located, RangeRead};
use crate::{notice, source_line, Res, TileSource, ATTRIBUTION};

/// What to load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub bounds: Bounds,
    /// The zooms wanted; the deepest is lowered to fit [`Want::cap`].
    pub zooms: RangeInclusive<u8>,
    /// The shallowest the deepest zoom may be lowered to.
    pub floor: u8,
    /// The most archive bytes the tiles may come to.
    pub cap: u64,
    pub profile: u8,
    /// The language names are kept in beside the local ones
    /// ([`CutOptions::locale`]).
    pub locale: Option<String>,
    /// Cut the buildings too ([`CutOptions::buildings`]).
    pub buildings: bool,
    /// Cut the pois and travel attributes too ([`CutOptions::pois`]).
    pub pois: bool,
}

/// What a load fetched and wrote.
#[derive(Debug)]
pub struct Loaded {
    /// The zooms the region holds.
    pub zooms: RangeInclusive<u8>,
    /// Archive bytes fetched, the gaps between tiles included.
    pub fetched: u64,
    /// Range requests for the tiles (the directories not counted).
    pub requests: usize,
    pub stats: CutStats,
}

/// The box's tiles would fetch more than the cap even at the floor zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    /// Archive bytes at the floor zoom.
    pub bytes: u64,
    pub cap: u64,
}

impl core::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "the area is {} MB of tiles, over the {} MB a load takes",
            self.bytes.div_ceil(1 << 20),
            self.cap >> 20
        )
    }
}

impl std::error::Error for TooLarge {}

/// The source holds no tile in the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Empty;

impl core::fmt::Display for Empty {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the map source holds no tiles there")
    }
}

impl std::error::Error for Empty {}

/// The deepest zoom of `zooms` down to `floor` whose tiles (each a zoom and
/// its length) come to at most `cap` bytes, with the bytes; `Err` with the
/// bytes at the floor when none does.
pub fn deepest_within(
    tiles: &[(u8, u32)],
    zooms: &RangeInclusive<u8>,
    floor: u8,
    cap: u64,
) -> Result<(u8, u64), u64> {
    let bytes = |hi: u8| -> u64 {
        tiles
            .iter()
            .filter(|(z, _)| *z <= hi)
            .map(|(_, l)| u64::from(*l))
            .sum()
    };
    let lowest = floor.clamp(*zooms.start(), *zooms.end());
    for hi in (lowest..=*zooms.end()).rev() {
        let b = bytes(hi);
        if b <= cap {
            return Ok((hi, b));
        }
    }
    Err(bytes(lowest))
}

/// Cut `want`'s box of `archive` into `out`, the region's notice naming
/// `by` as the program that cut it.
pub fn load<R: RangeRead, W: Write + Seek>(
    archive: &Archive<R>,
    want: &Want,
    by: &str,
    threads: usize,
    out: W,
) -> Res<Loaded> {
    let planned = plan(archive, want)?;
    cut_planned(archive, want, planned, by, threads, out, &mut |_| {})
}

/// The tiles a load fetches, located and under the cap, before any is
/// fetched: what names the region's zooms before it is written.
#[derive(Debug)]
pub struct Planned {
    /// The zooms the region holds.
    pub zooms: RangeInclusive<u8>,
    chosen: Vec<Located>,
}

/// Locate `want`'s tiles in `archive` and lower the deepest zoom until
/// they fit the cap.
pub fn plan<R: RangeRead>(archive: &Archive<R>, want: &Want) -> Res<Planned> {
    let located = archive.locate_in(&want.bounds, want.zooms.clone())?;
    if located.is_empty() {
        return Err(Box::new(Empty));
    }
    let sizes: Vec<(u8, u32)> = located.iter().map(|l| (l.0.z, l.2)).collect();
    let (hi, _) =
        deepest_within(&sizes, &want.zooms, want.floor, want.cap).map_err(|bytes| TooLarge {
            bytes,
            cap: want.cap,
        })?;
    Ok(Planned {
        zooms: *want.zooms.start()..=hi,
        chosen: located.into_iter().filter(|l| l.0.z <= hi).collect(),
    })
}

/// The planned tiles of an archive, fetched a round of the cut at a time
/// ([`TileSource::want`]): the round the cut works on is all it holds.
struct Arriving<'a, R: RangeRead> {
    archive: &'a Archive<R>,
    chosen: HashMap<TileId, Located>,
    round: RwLock<Option<Fetched>>,
    bytes: AtomicU64,
    requests: AtomicUsize,
}

impl<R: RangeRead> TileSource for Arriving<'_, R> {
    fn tiles_in(&self, bounds: &Bounds, zooms: RangeInclusive<u8>) -> Res<Vec<TileId>> {
        Ok(self
            .chosen
            .keys()
            .copied()
            .filter(|t| zooms.contains(&t.z) && in_bounds(*t, bounds))
            .collect())
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        let round = self.round.read().unwrap_or_else(PoisonError::into_inner);
        match round.as_ref() {
            Some(fetched) => fetched.mvt(t),
            None => Ok(None),
        }
    }

    fn want(&self, tiles: &[TileId]) -> Res<()> {
        let located: Vec<Located> = tiles
            .iter()
            .filter_map(|t| self.chosen.get(t).copied())
            .collect();
        let fetched = self.archive.fetch(&located)?;
        self.bytes.fetch_add(fetched.bytes, Ordering::Relaxed);
        self.requests.fetch_add(fetched.requests, Ordering::Relaxed);
        *self.round.write().unwrap_or_else(PoisonError::into_inner) = Some(fetched);
        Ok(())
    }
}

/// How far a load has come (1836): tiles and archive bytes, done of
/// planned. The bytes are the tiles' own, the gaps a range spans aside,
/// so done reaches planned exactly when every tile is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadProgress {
    pub tiles_done: usize,
    pub tiles: usize,
    pub bytes_done: u64,
    pub bytes: u64,
    /// The deepest zoom written so far.
    pub zoom: u8,
    /// The region is complete ([`Progress::complete`]).
    pub complete: bool,
}

/// A load that lives across calls (1836): the plan, the archive's tiles
/// still to fetch and the region file as far as it is written. Each
/// [`Loading::step`] fetches and writes one round; between two steps the
/// caller may pause as long as it likes and go on later from the same
/// round, the file a painter draws from staying as that round left it.
/// [`cut_planned`] is the same in one call.
pub struct Loading<'a, R: RangeRead, W: Write + Seek> {
    fetched: Arriving<'a, R>,
    cut: Streaming<W>,
    zooms: RangeInclusive<u8>,
    /// Every tile's length, by id: the bytes a round brings.
    lengths: HashMap<TileId, u64>,
    bytes: u64,
    bytes_done: u64,
    zoom: u8,
}

impl<'a, R: RangeRead, W: Write + Seek> Loading<'a, R, W> {
    /// Write `planned`'s region header and slots into `out`; nothing is
    /// fetched yet.
    pub fn begin(
        archive: &'a Archive<R>,
        want: &Want,
        planned: Planned,
        by: &str,
        threads: usize,
        out: W,
    ) -> Res<Self> {
        let zooms = planned.zooms;
        let first = *zooms.start();
        let lengths: HashMap<TileId, u64> = planned
            .chosen
            .iter()
            .map(|l| (l.0, u64::from(l.2)))
            .collect();
        let fetched = Arriving {
            archive,
            chosen: planned.chosen.into_iter().map(|l| (l.0, l)).collect(),
            round: RwLock::new(None),
            bytes: AtomicU64::new(0),
            requests: AtomicUsize::new(0),
        };
        let opts = CutOptions {
            bounds: want.bounds,
            zooms: zooms.clone(),
            profile: want.profile,
            attribution: ATTRIBUTION.to_owned(),
            notice: notice(by, want.profile, &source_line(archive)),
            threads,
            locale: want.locale.clone(),
            buildings: want.buildings,
            pois: want.pois,
            round: crate::cut::ROUND,
        };
        let cut = Streaming::begin(&fetched, &opts, out)?;
        // The cut's own list decides what is fetched: a tile the plan
        // located that the cut drops (out of the box) brings no byte.
        let bytes = cut
            .tiles()
            .iter()
            .map(|t| lengths.get(t).copied().unwrap_or(0))
            .sum();
        Ok(Self {
            fetched,
            cut,
            zooms,
            lengths,
            bytes,
            bytes_done: 0,
            zoom: first,
        })
    }

    /// Where the load stands.
    pub fn progress(&self) -> LoadProgress {
        LoadProgress {
            tiles_done: self.cut.done(),
            tiles: self.cut.tiles().len(),
            bytes_done: self.bytes_done,
            bytes: self.bytes,
            zoom: self.zoom,
            complete: false,
        }
    }

    /// Whether a round is left to fetch.
    pub fn rounds_left(&self) -> bool {
        self.cut.next_round().is_some()
    }

    /// Fetch and write the next round; its progress, or `None` when every
    /// round is written and only [`finish`](Self::finish) is left. The
    /// round fetched is dropped once written: a paused load holds no tile.
    pub fn step(&mut self) -> Res<Option<LoadProgress>> {
        let round: u64 = match self.cut.next_round() {
            Some(round) => round
                .iter()
                .map(|t| self.lengths.get(t).copied().unwrap_or(0))
                .sum(),
            None => return Ok(None),
        };
        let written = self.cut.round(&self.fetched)?;
        *self
            .fetched
            .round
            .write()
            .unwrap_or_else(PoisonError::into_inner) = None;
        Ok(written.map(|p| {
            self.bytes_done += round;
            self.zoom = p.zoom;
            self.progress()
        }))
    }

    /// Write the names, buildings, pois and the length: the region is
    /// complete. Every round must have been stepped.
    pub fn finish(self) -> Res<Loaded> {
        let (stats, _) = self.cut.finish()?;
        Ok(Loaded {
            zooms: self.zooms,
            fetched: self.fetched.bytes.load(Ordering::Relaxed),
            requests: self.fetched.requests.load(Ordering::Relaxed),
            stats,
        })
    }
}

/// Fetch `planned`'s tiles and cut them into `out`, a round at a time,
/// telling `progress` after each round is written and once complete
/// ([`Loading`] in one call).
pub fn cut_planned<R: RangeRead, W: Write + Seek>(
    archive: &Archive<R>,
    want: &Want,
    planned: Planned,
    by: &str,
    threads: usize,
    out: W,
    progress: &mut dyn FnMut(LoadProgress),
) -> Res<Loaded> {
    let mut loading = Loading::begin(archive, want, planned, by, threads, out)?;
    while let Some(p) = loading.step()? {
        progress(p);
    }
    let last = loading.progress();
    let loaded = loading.finish()?;
    progress(LoadProgress {
        complete: true,
        zoom: *loaded.zooms.end(),
        ..last
    });
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");

    struct InMemory;

    impl RangeRead for InMemory {
        fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
            let at = (offset as usize).min(FIXTURE.len());
            let end = at.saturating_add(len as usize).min(FIXTURE.len());
            Ok(FIXTURE[at..end].to_vec())
        }
    }

    /// A load paused between two rounds holds none of the tiles it
    /// fetched (1836): the round is written and let go, so four paused
    /// loads cost their plans, not their last rounds' bytes.
    #[test]
    fn a_load_between_rounds_holds_no_fetched_tile() {
        let archive = Archive::from_source(InMemory).expect("open");
        let want = Want {
            bounds: Bounds {
                west: 112_615_000,
                south: 437_405_000,
                east: 112_825_000,
                north: 437_545_000,
            },
            zooms: 0..=15,
            floor: 10,
            cap: 64 << 20,
            profile: 2,
            locale: None,
            buildings: false,
            pois: false,
        };
        let planned = plan(&archive, &want).expect("plan");
        let mut loading =
            Loading::begin(&archive, &want, planned, "test", 1, Cursor::new(Vec::new()))
                .expect("begin");
        let held = |l: &Loading<'_, InMemory, Cursor<Vec<u8>>>| {
            l.fetched
                .round
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
        };
        assert!(!held(&loading));
        while loading.step().expect("round").is_some() {
            assert!(!held(&loading), "a written round is let go");
        }
    }
}
