//! `lnflash --ble <on|off> --port <tty>` (1480): switch a ThinkNode M9's
//! BLE carrier.
//!
//! BLE is on by default (the zero-config decision, #471): the record
//! (`leviculum_core::media_switches`) exists to switch a carrier off, and
//! `--ble on` clears the opt-out again. It goes into the board's `media`
//! partition at 0xD000 (`leviculum-esp/partitions-m9.csv`) through the
//! ROM loader, as the WLAN record does (1479); the board reads it at
//! boot. The nRF boards take the same decision over the control envelope
//! instead (`--set-media`, [`crate::media`]): their firmware answers
//! `TYPE_MEDIA_PROFILE` live, the M9's does not yet, and the ROM loader
//! is the M9's proven way in for every record it holds.

use std::path::Path;

use leviculum_core::media_switches::{MediaSwitches, RECORD_LEN};

use crate::maps;

/// Where the record lives in the M9's flash.
pub const MEDIA_OFFSET: u32 = 0xD000;

/// The sector as it is written: the record, the rest erased.
pub fn sector_of(ble_on: bool) -> Vec<u8> {
    let record = MediaSwitches::all_on().set_ble(ble_on);
    let mut sector = vec![0xFF; 0x1000];
    sector[..RECORD_LEN].copy_from_slice(&record.encode());
    sector
}

/// `--ble`: write the switch into the M9 on `port`. What to say.
pub fn write(port: &Path, ble_on: bool) -> Result<String, String> {
    let sector = sector_of(ble_on);
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let path = dir.path().join("media.bin");
    std::fs::write(&path, &sector).map_err(|e| format!("{}: {e}", path.display()))?;
    maps::run(&maps::write_at_args(port, MEDIA_OFFSET, &path))?;
    let said = if ble_on {
        "BLE is on again (the default)"
    } else {
        "BLE stays off from the next boot"
    };
    Ok(format!(
        "{}: {said}; the board resets and its [BLE ] line says so",
        port.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_sector_holds_a_record_the_board_reads_back() {
        let sector = sector_of(false);
        assert_eq!(sector.len(), 0x1000);
        let back = MediaSwitches::decode(&sector).expect("decodes");
        assert!(!back.ble());
        assert!(sector[RECORD_LEN..].iter().all(|&b| b == 0xFF));
        assert!(MediaSwitches::decode(&sector_of(true))
            .expect("decodes")
            .ble());
    }

    /// espflash writes where the partition table puts the `media` entry.
    #[test]
    fn espflash_writes_where_the_table_puts_the_media_entry() {
        let args: Vec<String> = maps::write_at_args(
            Path::new("/dev/ttyUSB0"),
            MEDIA_OFFSET,
            Path::new("/x/m.bin"),
        )
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            args.join(" "),
            "write-bin --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xd000 /x/m.bin"
        );
        let table = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-esp/partitions-m9.csv"),
        )
        .expect("the table");
        let row = table
            .lines()
            .find(|l| l.starts_with("media,"))
            .expect("a media row");
        let cells: Vec<&str> = row.split(',').map(str::trim).collect();
        assert_eq!(cells[3], format!("{MEDIA_OFFSET:#x}"));
        assert_eq!(cells[4], "0x1000");
    }
}
