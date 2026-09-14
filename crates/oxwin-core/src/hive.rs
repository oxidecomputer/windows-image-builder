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
#[allow(dead_code)]
pub(crate) const BASE: usize = 4096;

#[allow(dead_code)]
pub(crate) struct BaseBlock {
    pub root_offset: u32,
    pub bins_size: u32,
}

/// XOR of the first 127 little-endian u32s. Zero and `!0` are reserved.
#[allow(dead_code)]
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

#[allow(dead_code)]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    w.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(w)
}

#[allow(dead_code)]
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
}
