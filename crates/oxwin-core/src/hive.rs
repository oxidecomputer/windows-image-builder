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
/// All items in this module have `#[allow(dead_code)]` until Task 6, when the
/// `builder` crate becomes their consumer.
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

#[allow(dead_code)]
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
#[allow(dead_code)]
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
        if bin_size == 0
            || !bin_size.is_multiple_of(BASE)
            || bin + bin_size > end
        {
            bail!("bin at {bin:#x} has an implausible size of {bin_size}");
        }
        let mut at = bin + 32;
        while at < bin + bin_size {
            let raw = i32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("4 bytes"),
            );
            let size = raw.unsigned_abs() as usize;
            if size == 0 {
                bail!(
                    "cell at {at:#x} has zero size: bin's cells do not tile \
                     (gap or misread)"
                );
            }
            if !size.is_multiple_of(8) {
                bail!("cell at {at:#x} has size {raw}");
            }
            if at + size > bin + bin_size {
                bail!("cell at {at:#x} runs past the end of its bin");
            }
            out.push(Cell { at, size, allocated: raw < 0 });
            at += size;
        }
        debug_assert_eq!(
            at,
            bin + bin_size,
            "loop invariant: cells tile the bin exactly"
        );
        bin += bin_size;
    }
    Ok(out)
}

/// Structural check, run on our own output before we hand it back.
#[allow(dead_code)]
pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    cells(bytes)?;
    Ok(())
}

/// A key node. Field offsets below are from the cell start, so the signature is
/// at `at + 4` and everything else follows the documented `nk` layout.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct Key {
    pub at: usize,
}

#[allow(dead_code)]
impl Key {
    pub fn subkey_count(&self, b: &[u8]) -> u32 {
        u32_at(b, self.at + 24)
    }
    pub fn subkey_list(&self, b: &[u8]) -> u32 {
        u32_at(b, self.at + 32)
    }
    pub fn value_count(&self, b: &[u8]) -> u32 {
        u32_at(b, self.at + 40)
    }
    pub fn value_list(&self, b: &[u8]) -> u32 {
        u32_at(b, self.at + 44)
    }
    pub fn security(&self, b: &[u8]) -> u32 {
        u32_at(b, self.at + 48)
    }
    /// The key's own last-written timestamp, which new children inherit so that
    /// nothing here ever reads a clock.
    pub fn timestamp(&self, b: &[u8]) -> u64 {
        u64::from_le_bytes(
            b[self.at + 8..self.at + 16].try_into().expect("8 bytes"),
        )
    }

    pub fn name(&self, b: &[u8]) -> Result<String> {
        let len = u16::from_le_bytes(
            b[self.at + 76..self.at + 78].try_into().expect("2 bytes"),
        ) as usize;
        let flags = u16::from_le_bytes(
            b[self.at + 6..self.at + 8].try_into().expect("2 bytes"),
        );
        let raw = &b[self.at + 80..self.at + 80 + len];
        if flags & 0x0020 != 0 {
            Ok(raw.iter().map(|&c| c as char).collect())
        } else {
            let wide: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(String::from_utf16_lossy(&wide))
        }
    }

    pub fn subkeys(&self, b: &[u8]) -> Result<Vec<(String, Key)>> {
        let list = self.subkey_list(b);
        if self.subkey_count(b) == 0 || list == u32::MAX {
            return Ok(Vec::new());
        }
        let at = BASE + list as usize;
        let sig = &b[at + 4..at + 6];
        if sig != b"lf" && sig != b"lh" {
            bail!(
                "subkey list at {at:#x} is {}, which this does not edit",
                String::from_utf8_lossy(sig)
            );
        }
        let count =
            u16::from_le_bytes(b[at + 6..at + 8].try_into().expect("2 bytes"))
                as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let off = u32_at(b, at + 8 + i * 8) as usize;
            let key = Key { at: BASE + off };
            out.push((key.name(b)?, key));
        }
        Ok(out)
    }

    pub fn value(&self, b: &[u8], name: &str) -> Result<Option<Vec<u8>>> {
        let count = self.value_count(b) as usize;
        if count == 0 {
            return Ok(None);
        }
        let list = BASE + self.value_list(b) as usize;
        for i in 0..count {
            let vk = BASE + u32_at(b, list + 4 + i * 4) as usize;
            let nlen = u16::from_le_bytes(
                b[vk + 6..vk + 8].try_into().expect("2 bytes"),
            ) as usize;
            let vk_flags = u16::from_le_bytes(
                b[vk + 20..vk + 22].try_into().expect("2 bytes"),
            );
            let raw_name = &b[vk + 24..vk + 24 + nlen];
            let this: String = if vk_flags & 0x0001 != 0 {
                raw_name.iter().map(|&c| c as char).collect()
            } else {
                let wide: Vec<u16> = raw_name
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&wide)
            };
            if this != name {
                continue;
            }
            let raw = u32_at(b, vk + 8);
            let len = (raw & 0x7fff_ffff) as usize;
            if raw & 0x8000_0000 != 0 {
                // Four bytes or fewer live in the offset field itself.
                return Ok(Some(b[vk + 12..vk + 12 + len.min(4)].to_vec()));
            }
            let data = BASE + u32_at(b, vk + 12) as usize + 4;
            return Ok(Some(b[data..data + len].to_vec()));
        }
        Ok(None)
    }
}

/// Cells are a multiple of 8 bytes and include their own 4-byte size header.
fn cell_size(body: usize) -> usize {
    (4 + body).div_ceil(8) * 8
}

/// Allocates a cell with room for `want` bytes of body and returns its hive
/// offset.
///
/// First fit over the free cells in file order — deterministic by
/// construction, which matters more here than packing efficiency. A
/// remainder of at least 8 bytes is left behind as a smaller free cell;
/// anything less is absorbed, because a cell cannot be smaller than its own
/// header.
/// This is dead code until Task 6 wires `builder` up as its first
/// non-test consumer.
#[allow(dead_code)]
pub(crate) fn alloc(bytes: &mut Vec<u8>, want: usize) -> Result<u32> {
    let need = cell_size(want);
    let found =
        cells(bytes)?.into_iter().find(|c| !c.allocated && c.size >= need);

    let at = match found {
        Some(cell) => {
            let remainder = cell.size - need;
            if remainder >= 8 {
                bytes[cell.at..cell.at + 4]
                    .copy_from_slice(&(-(need as i32)).to_le_bytes());
                let tail = cell.at + need;
                bytes[tail..tail + 4]
                    .copy_from_slice(&(remainder as i32).to_le_bytes());
            } else {
                bytes[cell.at..cell.at + 4]
                    .copy_from_slice(&(-(cell.size as i32)).to_le_bytes());
            }
            cell.at
        }
        None => {
            // No room: append a bin. One is always enough, because a single
            // element is far smaller than 4096 bytes, but size it anyway.
            let head = base_block(bytes)?;
            let bin_at = BASE + head.bins_size as usize;
            let bin_size = (32 + need).div_ceil(BASE) * BASE;
            let mut bin = vec![0u8; 32];
            bin[0..4].copy_from_slice(b"hbin");
            bin[4..8].copy_from_slice(&head.bins_size.to_le_bytes());
            bin[8..12].copy_from_slice(&(bin_size as u32).to_le_bytes());
            bin.resize(bin_size, 0);
            // The whole bin is one free cell; the split below claims part
            // of it.
            bin[32..36]
                .copy_from_slice(&((bin_size - 32) as i32).to_le_bytes());
            bytes.truncate(bin_at);
            bytes.extend_from_slice(&bin);

            let new_size = head.bins_size as usize + bin_size;
            bytes[40..44].copy_from_slice(&(new_size as u32).to_le_bytes());

            let cell_at = bin_at + 32;
            let remainder = (bin_size - 32) - need;
            bytes[cell_at..cell_at + 4]
                .copy_from_slice(&(-(need as i32)).to_le_bytes());
            if remainder >= 8 {
                let tail = cell_at + need;
                bytes[tail..tail + 4]
                    .copy_from_slice(&(remainder as i32).to_le_bytes());
            }
            cell_at
        }
    };

    // Zero the body so an allocation never carries stale bytes.
    let size =
        i32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
            .unsigned_abs() as usize;
    for b in &mut bytes[at + 4..at + size] {
        *b = 0;
    }

    let sum = checksum(bytes);
    bytes[508..512].copy_from_slice(&sum.to_le_bytes());
    Ok((at - BASE) as u32)
}

#[allow(dead_code)]
pub(crate) fn root(b: &[u8]) -> Result<Key> {
    Ok(Key { at: BASE + base_block(b)?.root_offset as usize })
}

/// Walks a path of subkey names from the root. `Ok(None)` means a name was not
/// found, which is a fact about the hive rather than an error.
#[allow(dead_code)]
pub(crate) fn find(b: &[u8], path: &[&str]) -> Result<Option<Key>> {
    let mut key = root(b)?;
    for want in path {
        let next = key
            .subkeys(b)?
            .into_iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(want));
        match next {
            Some((_, k)) => key = k,
            None => return Ok(None),
        }
    }
    Ok(Some(key))
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
    fn rejects_a_cell_that_leaves_a_gap() {
        let mut v = bare_hive_with_free_bin();
        // Write a single cell of 64 bytes instead of the full 4064 bytes.
        // The residual bytes (4096 - 32 - 64 = 4000 bytes) are unwritten, so the
        // walk reads them as a cell with size 0 at the gap offset, which is how
        // cells-do-not-tile is detected: a zero header means the bin is incomplete.
        v[BASE + 32..BASE + 36].copy_from_slice(&64i32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        let result = validate(&v);
        assert!(result.is_err());
        // Verify it fails on the gap (zero header) with the gap offset in the message.
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("zero size"),
            "should fail on gap's zero header, got: {msg}"
        );
    }

    #[test]
    fn rejects_a_cell_that_runs_past_the_bin() {
        let mut v = bare_hive_with_free_bin();
        // Write a cell whose size extends past the bin boundary.
        // Cell at offset 32: size 4080 (fills most of the bin)
        // But the bin is only 4096 bytes (offsets 0-4096 within the bin),
        // so the cell extends to 4128 + 4080 = 8208, past the bin end at 8192.
        v[BASE + 32..BASE + 36].copy_from_slice(&4080i32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        let result = validate(&v);
        assert!(result.is_err());
        // Verify it fails on the overrun check.
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("past the end"),
            "should fail on cell overrun, got: {msg}"
        );
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

    /// Builds a hive shaped like a real BCD store. `free_tail` is how many bytes
    /// of free cell to leave, so a caller can produce one with room to grow and
    /// one without.
    mod fixture {
        use super::super::BASE;

        pub const EMS_GUID: &str = "{0ce4991b-e6b3-4b16-b23c-5e0d9250e5d9}";

        struct Writer {
            cells: Vec<u8>, // everything after the 32-byte bin header
        }

        impl Writer {
            fn new() -> Self {
                Writer { cells: Vec::new() }
            }

            /// Appends a cell with `body` after the 4-byte size header, returns
            /// the hive offset (relative to the end of the base block).
            fn cell(&mut self, body: &[u8]) -> u32 {
                let size = (4 + body.len()).div_ceil(8) * 8;
                let at = 32 + self.cells.len();
                self.cells.extend_from_slice(&(-(size as i32)).to_le_bytes());
                self.cells.extend_from_slice(body);
                self.cells.resize(at - 32 + size, 0);
                at as u32
            }

            fn sk(&mut self) -> u32 {
                let mut b = Vec::new();
                b.extend_from_slice(b"sk");
                b.extend_from_slice(&0u16.to_le_bytes()); // reserved
                b.extend_from_slice(&0u32.to_le_bytes()); // flink
                b.extend_from_slice(&0u32.to_le_bytes()); // blink
                b.extend_from_slice(&1u32.to_le_bytes()); // reference count
                b.extend_from_slice(&4u32.to_le_bytes()); // descriptor size
                b.extend_from_slice(&[0u8; 4]); // a stub descriptor
                self.cell(&b)
            }

            /// One `vk` named "Element" holding `data` as REG_BINARY, plus its
            /// data cell and a one-entry value list. Returns the list offset.
            fn element_value(&mut self, data: &[u8]) -> u32 {
                let data_at = {
                    let mut b = Vec::new();
                    b.extend_from_slice(data);
                    self.cell(&b)
                };
                let vk = {
                    let name = b"Element";
                    let mut b = Vec::new();
                    b.extend_from_slice(b"vk");
                    b.extend_from_slice(&(name.len() as u16).to_le_bytes());
                    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
                    b.extend_from_slice(&data_at.to_le_bytes());
                    b.extend_from_slice(&3u32.to_le_bytes()); // REG_BINARY
                    b.extend_from_slice(&1u16.to_le_bytes()); // ASCII name
                    b.extend_from_slice(&0u16.to_le_bytes()); // spare
                    b.extend_from_slice(name);
                    self.cell(&b)
                };
                self.cell(&vk.to_le_bytes())
            }

            #[allow(clippy::too_many_arguments)]
            fn nk(
                &mut self,
                name: &str,
                parent: u32,
                sk: u32,
                subkeys: &[u32],
                subkey_list: u32,
                values: u32,
                value_list: u32,
            ) -> u32 {
                let mut b = Vec::new();
                b.extend_from_slice(b"nk");
                b.extend_from_slice(&0x0020u16.to_le_bytes()); // ASCII name
                b.extend_from_slice(&0u64.to_le_bytes()); // timestamp
                b.extend_from_slice(&0u32.to_le_bytes()); // access bits
                b.extend_from_slice(&parent.to_le_bytes());
                b.extend_from_slice(&(subkeys.len() as u32).to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes()); // volatile subkeys
                b.extend_from_slice(&subkey_list.to_le_bytes());
                b.extend_from_slice(&u32::MAX.to_le_bytes()); // volatile list
                b.extend_from_slice(&values.to_le_bytes());
                b.extend_from_slice(&value_list.to_le_bytes());
                b.extend_from_slice(&sk.to_le_bytes());
                b.extend_from_slice(&u32::MAX.to_le_bytes()); // class
                b.extend_from_slice(&[0u8; 20]); // largest-* and workvar
                b.extend_from_slice(&(name.len() as u16).to_le_bytes());
                b.extend_from_slice(&0u16.to_le_bytes()); // class length
                b.extend_from_slice(name.as_bytes());
                self.cell(&b)
            }

            /// An `lf` leaf over already-written subkeys, sorted by name.
            fn lf(&mut self, mut kids: Vec<(String, u32)>) -> u32 {
                kids.sort_by(|a, b| a.0.cmp(&b.0));
                let mut b = Vec::new();
                b.extend_from_slice(b"lf");
                b.extend_from_slice(&(kids.len() as u16).to_le_bytes());
                for (name, at) in kids {
                    b.extend_from_slice(&at.to_le_bytes());
                    let mut hint = [0u8; 4];
                    for (i, c) in name.bytes().take(4).enumerate() {
                        hint[i] = c;
                    }
                    b.extend_from_slice(&hint);
                }
                self.cell(&b)
            }
        }

        /// A hive shaped like a BCD store, with exactly `free_tail` bytes left
        /// free in the trailing free cell.
        ///
        /// Rounding the bin up to a whole 4096-byte multiple can leave more
        /// slack than `free_tail` asked for — up to 4095 bytes of it — which
        /// would silently defeat Task 4's test that allocation appends a new
        /// bin when there is no room. Any slack beyond exactly `free_tail` is
        /// therefore consumed by an allocated filler cell, so the trailing
        /// free cell is always precisely the requested size. `free_tail` must
        /// be a multiple of 8, matching every caller.
        pub fn bcd_like(free_tail: usize) -> Vec<u8> {
            assert!(
                free_tail.is_multiple_of(8),
                "free_tail must be a multiple of 8, got {free_tail}"
            );
            let mut w = Writer::new();
            let sk = w.sk();

            // \Objects\{emssettings}\Elements\16000020 — bootems, as shipped.
            let bootems_values = w.element_value(&[1]);
            let bootems =
                w.nk("16000020", 0, sk, &[], u32::MAX, 1, bootems_values);
            let elements_list = w.lf(vec![("16000020".into(), bootems)]);
            let elements =
                w.nk("Elements", 0, sk, &[bootems], elements_list, 0, u32::MAX);

            let desc_values = w.element_value(&[0x00, 0x00, 0x10, 0x20]);
            let description =
                w.nk("Description", 0, sk, &[], u32::MAX, 1, desc_values);

            let obj_list = w.lf(vec![
                ("Description".into(), description),
                ("Elements".into(), elements),
            ]);
            let object = w.nk(
                EMS_GUID,
                0,
                sk,
                &[description, elements],
                obj_list,
                0,
                u32::MAX,
            );

            let objects_list = w.lf(vec![(EMS_GUID.into(), object)]);
            let objects =
                w.nk("Objects", 0, sk, &[object], objects_list, 0, u32::MAX);
            let root_list = w.lf(vec![("Objects".into(), objects)]);
            let root =
                w.nk("System", 0, sk, &[objects], root_list, 0, u32::MAX);

            // Pad to a whole number of bins, leaving EXACTLY `free_tail` bytes
            // free. Any leftover between the last real cell and the free tail
            // becomes an allocated filler cell, so the bin still tiles exactly.
            // `used` is a sum of cell sizes each rounded up to a multiple of 8
            // (`Writer::cell`), `free_tail` is asserted to be a multiple of 8
            // above, and `bin_size` is a multiple of `BASE`, itself a
            // multiple of 8 — so `slack` can only ever be 0 or >= 8, never a
            // value too small to hold a cell header. No growth branch is
            // needed to make room for the filler.
            let used = 32 + w.cells.len();
            let bin_size = (used + free_tail).div_ceil(BASE) * BASE;
            let slack = bin_size - used - free_tail;
            debug_assert!(slack == 0 || slack >= 8, "slack must be 0 or >= 8");
            if slack > 0 {
                w.cells.extend_from_slice(&(-(slack as i32)).to_le_bytes());
                w.cells.resize(w.cells.len() + slack - 4, 0);
            }
            w.cells.extend_from_slice(&(free_tail as i32).to_le_bytes());
            w.cells.resize(bin_size - 32, 0);

            let mut v = vec![0u8; BASE];
            v[0..4].copy_from_slice(b"regf");
            v[4..8].copy_from_slice(&1u32.to_le_bytes());
            v[8..12].copy_from_slice(&1u32.to_le_bytes());
            v[20..24].copy_from_slice(&1u32.to_le_bytes());
            v[24..28].copy_from_slice(&3u32.to_le_bytes());
            v[28..32].copy_from_slice(&0u32.to_le_bytes());
            v[32..36].copy_from_slice(&1u32.to_le_bytes());
            v[36..40].copy_from_slice(&root.to_le_bytes());
            v[40..44].copy_from_slice(&(bin_size as u32).to_le_bytes());

            let mut bin = vec![0u8; 32];
            bin[0..4].copy_from_slice(b"hbin");
            bin[4..8].copy_from_slice(&0u32.to_le_bytes());
            bin[8..12].copy_from_slice(&(bin_size as u32).to_le_bytes());
            bin.extend_from_slice(&w.cells);
            v.extend_from_slice(&bin);

            let sum = super::super::checksum(&v);
            v[508..512].copy_from_slice(&sum.to_le_bytes());
            v
        }
    }

    #[test]
    fn the_fixture_is_a_valid_hive() {
        validate(&fixture::bcd_like(512)).unwrap();
    }

    #[test]
    fn walks_to_the_elements_key() {
        let v = fixture::bcd_like(512);
        let key = find(&v, &["Objects", fixture::EMS_GUID, "Elements"])
            .unwrap()
            .expect("Elements exists");
        let kids = key.subkeys(&v).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].0, "16000020");
    }

    #[test]
    fn reads_an_element_value() {
        let v = fixture::bcd_like(512);
        let key =
            find(&v, &["Objects", fixture::EMS_GUID, "Elements", "16000020"])
                .unwrap()
                .expect("bootems exists");
        assert_eq!(key.value(&v, "Element").unwrap(), Some(vec![1]));
    }

    #[test]
    fn a_missing_path_is_none_not_an_error() {
        let v = fixture::bcd_like(512);
        assert!(find(&v, &["Objects", "{nope}"]).unwrap().is_none());
    }

    /// R4: `bcd_like` must leave EXACTLY `free_tail` bytes free, not "at least".
    /// Task 4's no-room-to-grow test relies on this being exact, since rounding
    /// up to a whole bin could otherwise leave thousands of spare bytes and let
    /// an allocation succeed in place when it should have to append a new bin.
    #[test]
    fn free_tail_is_exact() {
        for free_tail in [512usize, 8] {
            let v = fixture::bcd_like(free_tail);
            let free: Vec<_> = cells(&v)
                .unwrap()
                .into_iter()
                .filter(|c| !c.allocated)
                .collect();
            assert_eq!(
                free.len(),
                1,
                "expected exactly one free cell for free_tail={free_tail}"
            );
            assert_eq!(
                free[0].size, free_tail,
                "free cell size should be exactly free_tail={free_tail}"
            );
        }
    }

    #[test]
    fn allocates_by_splitting_the_free_cell() {
        let mut v = fixture::bcd_like(512);
        let before = v.len();
        let at = alloc(&mut v, 16).unwrap();
        assert_eq!(v.len(), before, "no bin should have been appended");
        let cell = cells(&v)
            .unwrap()
            .into_iter()
            .find(|c| c.at == BASE + at as usize)
            .expect("the new cell exists");
        assert!(cell.allocated);
        assert!(cell.size >= 20);
        validate(&v).unwrap();
    }

    #[test]
    fn appends_a_bin_when_there_is_no_room() {
        // Eight bytes of tail: enough for a free cell header, not for a
        // request.
        let mut v = fixture::bcd_like(8);
        let before = v.len();
        let at = alloc(&mut v, 256).unwrap();
        assert_eq!(v.len(), before + BASE, "exactly one bin appended");
        assert!(at as usize > 0);
        validate(&v).unwrap();
    }

    #[test]
    fn a_split_leaves_the_remainder_free() {
        let mut v = fixture::bcd_like(512);
        alloc(&mut v, 16).unwrap();
        let free: usize =
            cells(&v).unwrap().iter().filter(|c| !c.allocated).count();
        assert_eq!(free, 1, "the tail remains as one free cell");
        validate(&v).unwrap();
    }

    /// Allocation must not depend on anything but the bytes, or two builds
    /// of the same image would differ.
    #[test]
    fn allocation_is_deterministic() {
        let mut a = fixture::bcd_like(512);
        let mut b = fixture::bcd_like(512);
        assert_eq!(alloc(&mut a, 24).unwrap(), alloc(&mut b, 24).unwrap());
        assert_eq!(a, b);
    }
}
