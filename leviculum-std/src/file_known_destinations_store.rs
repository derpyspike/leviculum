//! File-backed known destinations store (msgpack, Python-compatible).

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};
use std::io::{BufReader, BufWriter, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::known_destinations::{KnownDestEntry, KnownDestUseState};

use crate::error::Error;
use crate::known_destinations::{
    disk_only_entry_is_kept, for_each_known_destination, write_known_destination,
    write_known_destinations_header, KNOWN_DESTINATIONS_FILE,
};

/// Buffer size of the streaming reader and writer.
const IO_BUFFER: usize = 64 * 1024;

pub(crate) struct FileKnownDestinationsStore {
    path: PathBuf,
}

impl FileKnownDestinationsStore {
    pub(crate) fn new(storage_dir: &Path) -> Self {
        Self {
            path: storage_dir.join(KNOWN_DESTINATIONS_FILE),
        }
    }

    /// Read the file one entry at a time, handing each valid entry to `f`.
    /// A missing file reads as empty.
    pub(crate) fn for_each(
        &self,
        f: impl FnMut([u8; TRUNCATED_HASHBYTES], KnownDestEntry) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Storage(format!("{e}"))),
        };
        let mut reader = BufReader::with_capacity(IO_BUFFER, file);
        for_each_known_destination(&mut reader, f).map(|_| ())
    }

    /// Write `held` and the entries only the file has, in memory bounded by
    /// `held` and `disk_only_cap`, not by the file (order 1749).
    ///
    /// The file can hold entries this node does not: another process on the
    /// same storage wrote them (Python recombines the same way,
    /// `Identity.py:197-206`), or this node held them once and let go of
    /// them at its memory cap. Such an entry stays if the reference's cull
    /// would keep it ([`disk_only_entry_is_kept`]), and of those at most
    /// `disk_only_cap`: the retained (`-1`) ones first, then the rest, each
    /// group newest by announce time first ([`Rank`]). The reference never
    /// culls a pin while a use only buys `DESTINATION_TIMEOUT * 1.25`, so a
    /// used or never-used entry never takes a pin's place (order 1767);
    /// pins beyond the ceiling go, the oldest announced first. A dropped
    /// entry comes back with the destination's next announce, as it would
    /// on an rnsd that culled it.
    ///
    /// Two passes over one open handle of the old file: the first counts
    /// what stays, because the msgpack map header carries the count, the
    /// second copies it. A writer that replaces the file meanwhile
    /// (`os.replace` in Python, the rename here) leaves the inode this
    /// handle reads untouched. The new file goes to a temp file beside the
    /// old one, synced, then renamed over it, so a kill at any point leaves
    /// one complete file.
    ///
    /// The production writers of the file in this workspace are this one
    /// (every flush of a node's storage) and loquax-android's mailbox
    /// move-in (`loquax-android/src/mailbox.rs`, `take_over`), which puts
    /// the file a `/move` delivered in place whole, through
    /// [`crate::storage::atomic_write_durable`]. Outside it, an rnsd on the
    /// same storage writes it too (`Identity.py:197-239`).
    pub(crate) fn merge_and_save(
        &self,
        held: &BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>,
        now: f64,
        disk_only_cap: usize,
    ) -> Result<(), Error> {
        let old = match std::fs::File::open(&self.path) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::Storage(format!("{e}"))),
        };
        let eligible = |hash: &[u8; TRUNCATED_HASHBYTES], entry: &KnownDestEntry| {
            !held.contains_key(hash) && disk_only_entry_is_kept(entry, now)
        };

        // Pass 1: the ranks of the highest `disk_only_cap` that stay.
        let mut newest = NewestRanks::new(disk_only_cap);
        let mut old = old.and_then(|file| {
            let mut reader = BufReader::with_capacity(IO_BUFFER, file);
            let counted = for_each_known_destination(&mut reader, |hash, entry| {
                if eligible(&hash, &entry) {
                    newest.offer(Rank::of(&entry));
                }
                Ok(())
            });
            match counted {
                Ok(_) => Some(reader.into_inner()),
                Err(e) => {
                    // As before: an unreadable file contributes nothing,
                    // and the write replaces it with what this node holds.
                    tracing::warn!("known_destinations unreadable, rewriting it: {e}");
                    newest = NewestRanks::new(disk_only_cap);
                    None
                }
            }
        });
        let mut selection = newest.selection();
        let disk_only = selection.count;

        let temp_path = self.path.with_extension("tmp");
        let written = (|| -> Result<(), Error> {
            let temp = std::fs::File::create(&temp_path)
                .map_err(|e| Error::Storage(format!("Failed to write temp file: {e}")))?;
            let mut writer = BufWriter::with_capacity(IO_BUFFER, temp);
            write_known_destinations_header(&mut writer, held.len() + disk_only)?;
            for (hash, entry) in held {
                write_known_destination(&mut writer, hash, entry)?;
            }

            // Pass 2: copy the entries pass 1 selected, in file order.
            if disk_only > 0 {
                let mut file = old
                    .take()
                    .ok_or_else(|| Error::Storage("known_destinations vanished".into()))?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|e| Error::Storage(format!("{e}")))?;
                let mut reader = BufReader::with_capacity(IO_BUFFER, file);
                let mut copied = 0usize;
                for_each_known_destination(&mut reader, |hash, entry| {
                    if eligible(&hash, &entry) && selection.take(Rank::of(&entry)) {
                        write_known_destination(&mut writer, &hash, &entry)?;
                        copied += 1;
                    }
                    Ok(())
                })?;
                if copied != disk_only {
                    return Err(Error::Storage(format!(
                        "known_destinations changed under the flush: \
                         {copied} entries copied, {disk_only} counted"
                    )));
                }
            }

            let file = writer
                .into_inner()
                .map_err(|e| Error::Storage(format!("Failed to write temp file: {e}")))?;
            file.sync_all()
                .map_err(|e| Error::Storage(format!("Failed to sync temp file: {e}")))?;
            Ok(())
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp_path);
            return Err(e);
        }
        std::fs::rename(&temp_path, &self.path)
            .map_err(|e| Error::Storage(format!("Failed to rename temp file: {e}")))?;
        Ok(())
    }
}

/// The rule both the load and the flush trim the table by: keep the `cap`
/// highest ranks offered; at the lowest kept rank (the floor), as many as
/// fill the cap, the first ones in the order they are offered again.
///
/// Pass 1 offers every candidate ([`NewestRanks::offer`]), which holds a
/// min-heap of `cap` ranks: memory in the cap, not in what is offered.
/// Pass 2 offers the same candidates in the same order to the resulting
/// [`Selection::take`]. Both passes order AND compare by `R`'s `Ord`, so
/// what pass 1 counted is exactly what pass 2 takes (order 1972: a derived
/// `PartialEq` on an `f64` disagreed with a `total_cmp` order on NaN, and
/// every flush of a file with NaN announce times at the floor was
/// abandoned).
pub(crate) struct NewestRanks<R> {
    cap: usize,
    heap: BinaryHeap<Reverse<R>>,
}

impl<R: Ord + Copy> NewestRanks<R> {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            heap: BinaryHeap::new(),
        }
    }

    pub(crate) fn offer(&mut self, rank: R) {
        if self.cap == 0 {
            return;
        }
        if self.heap.len() < self.cap {
            self.heap.push(Reverse(rank));
        } else if self
            .heap
            .peek()
            .is_some_and(|Reverse(lowest)| rank.cmp(lowest) == Ordering::Greater)
        {
            self.heap.pop();
            self.heap.push(Reverse(rank));
        }
    }

    /// What pass 2 takes: everything offered while fewer than `cap` were;
    /// otherwise everything ranked above the lowest kept rank, and as many
    /// at that rank as fill the cap.
    pub(crate) fn selection(self) -> Selection<R> {
        if self.heap.len() < self.cap {
            return Selection {
                floor: None,
                room_at_floor: 0,
                count: self.heap.len(),
            };
        }
        let Some(Reverse(floor)) = self.heap.peek().copied() else {
            // Only a zero cap leaves a full heap empty: nothing is taken.
            return Selection {
                floor: None,
                room_at_floor: 0,
                count: 0,
            };
        };
        let above = self
            .heap
            .iter()
            .filter(|Reverse(r)| r.cmp(&floor) == Ordering::Greater)
            .count();
        Selection {
            floor: Some(floor),
            room_at_floor: self.cap - above,
            count: self.cap,
        }
    }
}

/// Pass 2 of [`NewestRanks`]: takes exactly `count` of the ranks offered in
/// pass 1, offered again in the same order.
pub(crate) struct Selection<R> {
    floor: Option<R>,
    room_at_floor: usize,
    /// How many [`Selection::take`] answers `true` for.
    pub(crate) count: usize,
}

impl<R: Ord> Selection<R> {
    /// Whether the next candidate, in pass-1 order, is taken.
    pub(crate) fn take(&mut self, rank: R) -> bool {
        let Some(floor) = &self.floor else {
            return self.count > 0;
        };
        match rank.cmp(floor) {
            Ordering::Greater => true,
            Ordering::Equal if self.room_at_floor > 0 => {
                self.room_at_floor -= 1;
                true
            }
            _ => false,
        }
    }
}

/// An announce time as a rank: ordered and compared by `f64::total_cmp`,
/// so a NaN read off disk equals itself and sorts above every number (a
/// positive NaN) or below (a negative one), the same way in every pass.
#[derive(Clone, Copy)]
pub(crate) struct Announced(pub(crate) f64);

impl PartialEq for Announced {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Announced {}

impl PartialOrd for Announced {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Announced {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Where a disk-only entry stands for the `disk_only_cap` places: every
/// retained one above every other, and within each group the later
/// announce above the earlier. Ordering and equality both follow `Ord`,
/// which compares the announce time with `f64::total_cmp`: two NaN
/// announce times are equal in both passes of [`NewestRanks`].
#[derive(Clone, Copy)]
struct Rank {
    retained: bool,
    announced: f64,
}

impl Rank {
    fn of(entry: &KnownDestEntry) -> Self {
        Self {
            retained: matches!(entry.use_state, KnownDestUseState::Retained),
            announced: entry.timestamp,
        }
    }
}

impl PartialEq for Rank {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Rank {}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.retained
            .cmp(&other.retained)
            .then(self.announced.total_cmp(&other.announced))
    }
}

/// The whole file decoded in one piece, for tests that read back what a
/// flush wrote. Production reads the file one entry at a time
/// ([`FileKnownDestinationsStore::for_each`]); this holds all of it.
#[cfg(test)]
impl FileKnownDestinationsStore {
    pub(crate) fn load_all(
        &self,
    ) -> Result<BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>, Error> {
        match std::fs::read(&self.path) {
            Ok(bytes) => crate::known_destinations::decode_known_destinations(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(Error::Storage(format!("{e}"))),
        }
    }
}
