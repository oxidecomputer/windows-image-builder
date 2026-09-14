// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! A minimal editor for Windows registry hives, for one job.
//!
//! Windows install media carries a BCD store — a registry hive — whose
//! `{emssettings}` object already enables Emergency Management Services but never
//! says which serial port to use. Windows then asks the firmware, via the ACPI
//! SPCR table, and on a guest without one it redirects to nothing. Two integer
//! elements fix it, and `EMS-SERIAL-INVESTIGATION.md` records how that was found.
//!
//! Scope is deliberately tiny. This is not a registry library: it walks to one
//! known object, adds two subkeys, and validates what it produced. Anything it
//! does not recognise it declines to touch, because the store it would be
//! corrupting is the one that boots the installer.

use anyhow::{Result, bail};

/// The base block is 4096 bytes, and every cell offset in the hive is relative to
/// the end of it.
pub(crate) const BASE: usize = 4096;

pub(crate) struct BaseBlock {
    pub root_offset: u32,
    pub bins_size: u32,
}

/// XOR of the first 127 little-endian u32s. Zero and `!0` are reserved.
pub(crate) fn checksum(bytes: &[u8]) -> u32 {
    let mut sum = 0u32;
    for i in 0..127 {
        let mut w = [0u8; 4];
        w.copy_from_slice(&bytes[i * 4..i * 4 + 4]);
        sum ^= u32::from_le_bytes(w);
    }
    match sum {
        0 => 1,
        u32::MAX => u32::MAX - 1,
        n => n,
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    w.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(w)
}

pub(crate) fn base_block(bytes: &[u8]) -> Result<BaseBlock> {
    if bytes.len() < BASE {
        bail!("not a hive: {} bytes, shorter than a base block", bytes.len());
    }
    if &bytes[0..4] != b"regf" {
        bail!("not a hive: no regf signature");
    }
    // Unequal sequence numbers mean a write was interrupted and the hive needs
    // recovery from a log. Editing one is not our business.
    if u32_at(bytes, 4) != u32_at(bytes, 8) {
        bail!("hive is dirty: sequence numbers differ");
    }
    if u32_at(bytes, 28) != 0 {
        bail!("not a primary hive");
    }
    let stored = u32_at(bytes, 508);
    let computed = checksum(bytes);
    if stored != computed {
        bail!("base block checksum is {stored:#x}, expected {computed:#x}");
    }
    let bins_size = u32_at(bytes, 40);
    if BASE + bins_size as usize > bytes.len() {
        bail!("hive claims {bins_size} bytes of bins, file is too short");
    }
    Ok(BaseBlock { root_offset: u32_at(bytes, 36), bins_size })
}

pub(crate) struct Cell {
    /// Absolute offset in the file, at the 4-byte size header.
    pub at: usize,
    /// Including the size header. Always a multiple of 8.
    pub size: usize,
    pub allocated: bool,
}

/// Every cell in the hive, in file order.
///
/// A bin's cells must tile it exactly: the format has no padding, so a gap means
/// we have misread something and must not write.
pub(crate) fn cells(bytes: &[u8]) -> Result<Vec<Cell>> {
    let head = base_block(bytes)?;
    let mut out = Vec::new();
    let mut bin = BASE;
    let end = BASE + head.bins_size as usize;
    while bin < end {
        if bytes.len() < bin + 32 || &bytes[bin..bin + 4] != b"hbin" {
            bail!("no hbin signature at {bin:#x}");
        }
        let bin_size = u32_at(bytes, bin + 8) as usize;
        if bin_size == 0 || bin_size % BASE != 0 || bin + bin_size > end {
            bail!("bin at {bin:#x} has an implausible size of {bin_size}");
        }
        let mut at = bin + 32;
        while at < bin + bin_size {
            let raw = i32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("4 bytes"),
            );
            let size = raw.unsigned_abs() as usize;
            if size == 0 || size % 8 != 0 {
                bail!("cell at {at:#x} has size {raw}");
            }
            if at + size > bin + bin_size {
                bail!("cell at {at:#x} runs past the end of its bin");
            }
            out.push(Cell { at, size, allocated: raw < 0 });
            at += size;
        }
        if at != bin + bin_size {
            bail!("cells do not tile the bin at {bin:#x}");
        }
        bin += bin_size;
    }
    Ok(out)
}

/// Structural check, run on our own output before we hand it back.
pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    cells(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_base(root: u32, bins: u32) -> Vec<u8> {
        let mut v = vec![0u8; 4096];
        v[0..4].copy_from_slice(b"regf");
        v[4..8].copy_from_slice(&1u32.to_le_bytes()); // primary sequence
        v[8..12].copy_from_slice(&1u32.to_le_bytes()); // secondary sequence
        v[20..24].copy_from_slice(&1u32.to_le_bytes()); // major version
        v[24..28].copy_from_slice(&3u32.to_le_bytes()); // minor version
        v[28..32].copy_from_slice(&0u32.to_le_bytes()); // primary file
        v[32..36].copy_from_slice(&1u32.to_le_bytes()); // direct memory load
        v[36..40].copy_from_slice(&root.to_le_bytes());
        v[40..44].copy_from_slice(&bins.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        v
    }

    #[test]
    fn reads_a_well_formed_base_block() {
        let b = base_block(&bare_base(32, 0)).unwrap();
        assert_eq!(b.root_offset, 32);
        assert_eq!(b.bins_size, 0);
    }

    #[test]
    fn rejects_a_file_that_is_not_a_hive() {
        let mut v = bare_base(32, 0);
        v[0..4].copy_from_slice(b"nope");
        assert!(base_block(&v).is_err());
    }

    #[test]
    fn rejects_a_bad_checksum() {
        let mut v = bare_base(32, 0);
        v[508..512].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        assert!(base_block(&v).is_err());
    }

    /// Sequence numbers differ on a hive that was interrupted mid-write. We will
    /// not edit one.
    #[test]
    fn rejects_a_dirty_hive() {
        let mut v = bare_base(32, 0);
        v[8..12].copy_from_slice(&2u32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(base_block(&v).is_err());
    }

    #[test]
    fn rejects_a_truncated_file() {
        assert!(base_block(&[0u8; 100]).is_err());
    }

    /// A base block plus one 4096-byte bin holding a single free cell.
    fn bare_hive_with_free_bin() -> Vec<u8> {
        let mut v = bare_base(32, 4096);
        let mut bin = vec![0u8; 4096];
        bin[0..4].copy_from_slice(b"hbin");
        bin[4..8].copy_from_slice(&0u32.to_le_bytes()); // offset of this bin
        bin[8..12].copy_from_slice(&4096u32.to_le_bytes()); // size
        // One free cell filling the rest of the bin. Positive size = free.
        let free = 4096i32 - 32;
        bin[32..36].copy_from_slice(&free.to_le_bytes());
        v.extend_from_slice(&bin);
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        v
    }

    #[test]
    fn walks_one_bin_of_one_free_cell() {
        let v = bare_hive_with_free_bin();
        let cells = cells(&v).unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].at, BASE + 32);
        assert_eq!(cells[0].size, 4096 - 32);
        assert!(!cells[0].allocated);
    }

    #[test]
    fn rejects_cells_that_do_not_tile_the_bin() {
        let mut v = bare_hive_with_free_bin();
        // Shrink the cell so it no longer reaches the end of the bin.
        v[BASE + 32..BASE + 36].copy_from_slice(&64i32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(validate(&v).is_err());
    }

    #[test]
    fn rejects_a_cell_size_that_is_not_a_multiple_of_eight() {
        let mut v = bare_hive_with_free_bin();
        v[BASE + 32..BASE + 36].copy_from_slice(&4060i32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(validate(&v).is_err());
    }

    #[test]
    fn a_well_formed_hive_validates() {
        validate(&bare_hive_with_free_bin()).unwrap();
    }
}
