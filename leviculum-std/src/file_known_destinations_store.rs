//! File-backed known destinations store (msgpack, Python-compatible).

use std::cmp::Reverse;
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
        let mut newest = NewestStamps::new(disk_only_cap);
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
                    newest = NewestStamps::new(disk_only_cap);
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

/// The ranks of the highest `cap` entries offered, held in a min-heap of
/// `cap` ranks: memory in the cap, not in the file.
struct NewestStamps {
    cap: usize,
    heap: BinaryHeap<Reverse<Rank>>,
}

/// Where a disk-only entry stands for the `disk_only_cap` places: every
/// retained one above every other, and within each group the later
/// announce above the earlier. The announce time orders by
/// `f64::total_cmp`, so a NaN read off disk orders the same way in both
/// passes.
#[derive(Clone, Copy, PartialEq)]
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

impl Eq for Rank {}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.retained
            .cmp(&other.retained)
            .then(self.announced.total_cmp(&other.announced))
    }
}

impl NewestStamps {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            heap: BinaryHeap::new(),
        }
    }

    fn offer(&mut self, stamp: Rank) {
        if self.cap == 0 {
            return;
        }
        if self.heap.len() < self.cap {
            self.heap.push(Reverse(stamp));
        } else if self
            .heap
            .peek()
            .is_some_and(|Reverse(oldest)| stamp > *oldest)
        {
            self.heap.pop();
            self.heap.push(Reverse(stamp));
        }
    }

    /// What pass 2 copies: everything offered while fewer than `cap` were;
    /// otherwise everything ranked above the lowest kept rank, and as many
    /// at that rank as fill the cap, the first ones in file order.
    fn selection(self) -> Selection {
        if self.heap.len() < self.cap {
            return Selection {
                floor: None,
                room_at_floor: 0,
                count: self.heap.len(),
            };
        }
        let Some(Reverse(floor)) = self.heap.peek().copied() else {
            // Only a zero cap leaves a full heap empty.
            return Selection {
                floor: None,
                room_at_floor: 0,
                count: 0,
            };
        };
        let above = self.heap.iter().filter(|Reverse(s)| *s > floor).count();
        Selection {
            floor: Some(floor),
            room_at_floor: self.cap - above,
            count: self.cap,
        }
    }
}

struct Selection {
    floor: Option<Rank>,
    room_at_floor: usize,
    count: usize,
}

impl Selection {
    /// Whether the next eligible entry, in file order, is copied.
    fn take(&mut self, stamp: Rank) -> bool {
        let Some(floor) = self.floor else {
            return true;
        };
        if stamp > floor {
            return true;
        }
        if stamp == floor && self.room_at_floor > 0 {
            self.room_at_floor -= 1;
            return true;
        }
        false
    }
}

/// The whole-file codec, for tests that read or seed the file in one piece.
/// Production code has one writer of the file, [`merge_and_save`]
/// (order 1767): this path holds the whole file in memory and replaced it
/// without an fsync, which is what order 1749 took out of the flush.
///
/// [`merge_and_save`]: FileKnownDestinationsStore::merge_and_save
#[cfg(test)]
impl leviculum_core::known_destinations::KnownDestinationsStore for FileKnownDestinationsStore {
    type Error = Error;

    fn load_all(
        &mut self,
    ) -> core::result::Result<BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>, Error> {
        match std::fs::read(&self.path) {
            Ok(bytes) => crate::known_destinations::decode_known_destinations(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(Error::Storage(format!("{e}"))),
        }
    }

    fn save_all(
        &mut self,
        entries: &BTreeMap<[u8; TRUNCATED_HASHBYTES], KnownDestEntry>,
    ) -> core::result::Result<(), Error> {
        let encoded = crate::known_destinations::encode_known_destinations(entries)?;
        crate::storage::atomic_write(&self.path, &encoded)
    }
}
