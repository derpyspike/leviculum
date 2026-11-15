//! A load that lives across calls (1836): the plan and the rounds written
//! stay between two calls, so a load whose area the user left pauses
//! between rounds, its file kept, and goes on later from where it was,
//! writing the same bytes as a load run in one go and fetching no tile
//! twice; and it says how far it is in bytes of bytes.

use std::cell::RefCell;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_map::region::{Bounds, Region, Slot};
use leviculum_mapcut::load::{cut_planned, plan, LoadProgress, Loading, Want};
use leviculum_mapcut::pmtiles::{Archive, RangeRead};
use leviculum_mapcut::Res;

const FIXTURE: &[u8] = include_bytes!("../fixtures/oltrarno.pmtiles");

/// The fixture, counting the bytes read from it.
#[derive(Clone, Default)]
struct Counted(Arc<AtomicU64>);

impl RangeRead for Counted {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let at = (offset as usize).min(FIXTURE.len());
        let end = at.saturating_add(len as usize).min(FIXTURE.len());
        self.0.fetch_add((end - at) as u64, Ordering::Relaxed);
        Ok(FIXTURE[at..end].to_vec())
    }
}

/// A file the load writes and the test reads between its steps.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Cursor<Vec<u8>>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().write(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Shared {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.0.borrow_mut().seek(to)
    }
}

impl Shared {
    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().get_ref().clone()
    }
}

fn want(bounds: Bounds) -> Want {
    Want {
        bounds,
        zooms: 0..=15,
        floor: 10,
        cap: 64 << 20,
        profile: 2,
        locale: None,
        buildings: true,
        pois: true,
    }
}

fn oltrarno() -> Bounds {
    Bounds {
        west: 112_615_000,
        south: 437_405_000,
        east: 112_825_000,
        north: 437_545_000,
    }
}

/// A corner of the same box: another load the paused one makes way for.
fn corner() -> Bounds {
    Bounds {
        west: 112_615_000,
        south: 437_405_000,
        east: 112_700_000,
        north: 437_460_000,
    }
}

/// The whole load in one call, its bytes and the archive bytes it read.
fn whole(bounds: Bounds) -> (Vec<u8>, u64, Vec<LoadProgress>) {
    let read = Counted::default();
    let archive = Archive::from_source(read.clone()).expect("open");
    let w = want(bounds);
    let planned = plan(&archive, &w).expect("plan");
    let before = read.0.load(Ordering::Relaxed);
    let mut out = Cursor::new(Vec::new());
    let mut seen = Vec::new();
    cut_planned(&archive, &w, planned, "test", 2, &mut out, &mut |p| {
        seen.push(p)
    })
    .expect("load");
    (
        out.into_inner(),
        read.0.load(Ordering::Relaxed) - before,
        seen,
    )
}

/// Tiles a reader of `bytes` finds here (filled or empty).
fn here(bytes: &[u8]) -> usize {
    let r = Region::from_bytes(bytes).expect("a paused file reads");
    (0..r.header().tiles)
        .filter(|n| !matches!(r.slot(*n).expect("slot").1, Slot::Pending))
        .count()
}

#[test]
fn a_load_paused_between_rounds_goes_on_where_it_was() {
    let (once, read_once, _) = whole(oltrarno());

    let read = Counted::default();
    let archive = Archive::from_source(read.clone()).expect("open");
    let w = want(oltrarno());
    let planned = plan(&archive, &w).expect("plan");
    let before = read.0.load(Ordering::Relaxed);
    let file = Shared::default();
    let mut paused = Loading::begin(&archive, &w, planned, "test", 2, file.clone()).expect("begin");
    // The fixture's box is two rounds, z14 and z15: pause after the first.
    assert!(paused.step().expect("round").is_some());
    let at_pause = paused.progress();
    assert!(at_pause.tiles_done > 0);
    assert!(paused.rounds_left(), "the fixture has more than one round");
    let kept = file.bytes();
    assert_eq!(
        here(&kept),
        at_pause.tiles_done,
        "the paused file shows the rounds written"
    );

    // Another load runs to its end while this one waits; the paused file
    // is untouched by it.
    let (_, _, other) = whole(corner());
    assert!(other.last().is_some_and(|p| p.complete));
    assert_eq!(file.bytes(), kept, "a paused load writes nothing");

    let mut after = Vec::new();
    while let Some(p) = paused.step().expect("round") {
        after.push(p);
    }
    paused.finish().expect("finish");
    // From where it was, not from the start: the first round after the
    // pause adds to the tiles written before it.
    assert!(after[0].tiles_done > at_pause.tiles_done, "{after:?}");
    assert_eq!(file.bytes(), once, "the same bytes as a load in one go");
    assert_eq!(
        read.0.load(Ordering::Relaxed) - before,
        read_once,
        "no round was fetched twice"
    );
}

#[test]
fn a_load_counts_bytes_of_bytes() {
    let (_, _, seen) = whole(oltrarno());
    let last = seen.last().expect("progress");
    assert!(last.complete);
    assert!(last.bytes > 0);
    assert_eq!(last.bytes_done, last.bytes, "every byte planned arrived");
    assert_eq!(last.tiles_done, last.tiles);
    assert!(seen.len() >= 3, "{seen:?}");
    for pair in seen.windows(2) {
        assert!(pair[0].bytes_done <= pair[1].bytes_done, "{seen:?}");
        assert_eq!(pair[0].bytes, pair[1].bytes, "the plan does not move");
    }
    // Bytes, not tiles: the shares differ where tiles differ in size.
    let first = seen[0];
    assert!(first.bytes_done > 0 && first.bytes_done < first.bytes);
    assert!(
        seen.iter()
            .any(|p| p.bytes_done * p.tiles as u64 != p.bytes * p.tiles_done as u64),
        "{seen:?}"
    );
}

#[test]
fn a_load_finishes_only_after_its_last_round() {
    let archive = Archive::from_source(Counted::default()).expect("open");
    let w = want(oltrarno());
    let planned = plan(&archive, &w).expect("plan");
    let mut loading =
        Loading::begin(&archive, &w, planned, "test", 2, Cursor::new(Vec::new())).expect("begin");
    loading.step().expect("round");
    assert!(loading.finish().is_err());
}
