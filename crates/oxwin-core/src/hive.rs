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

use anyhow::{Result, anyhow, bail};

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

/// Unchecked: only for offsets whose validity is the very thing being
/// established (the base block's own fixed fields, read before anything has
/// been trusted). Everything read after that uses `checked_u32` and its
/// siblings below, which return `Err` instead of panicking on a bad offset.
fn u32_at_unchecked(bytes: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    w.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(w)
}

/// Bounds-checked reads, for offsets that came out of the file rather than
/// from validated structure. `Key`'s accessors use these so a malformed or
/// hostile store makes them return `Err` rather than panic — the media this
/// module reads is third-party bytes nobody has vouched for, and the whole
/// point of `Outcome` over `Result` in `enable_ems` is that a store we
/// cannot edit must still boot, which only holds if a bad offset can never
/// unwind out of the crate.
fn get(b: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    b.get(at..at + len)
        .ok_or_else(|| anyhow!("offset {at:#x}+{len} is out of range"))
}

fn checked_u16(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(get(b, at, 2)?.try_into().expect("2 bytes")))
}

fn checked_u32(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(get(b, at, 4)?.try_into().expect("4 bytes")))
}

fn checked_u64(b: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(get(b, at, 8)?.try_into().expect("8 bytes")))
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
    if u32_at_unchecked(bytes, 4) != u32_at_unchecked(bytes, 8) {
        bail!("hive is dirty: sequence numbers differ");
    }
    if u32_at_unchecked(bytes, 28) != 0 {
        bail!("not a primary hive");
    }
    let stored = u32_at_unchecked(bytes, 508);
    let computed = checksum(bytes);
    if stored != computed {
        bail!("base block checksum is {stored:#x}, expected {computed:#x}");
    }
    let bins_size = u32_at_unchecked(bytes, 40);
    if BASE + bins_size as usize > bytes.len() {
        bail!("hive claims {bins_size} bytes of bins, file is too short");
    }
    Ok(BaseBlock { root_offset: u32_at_unchecked(bytes, 36), bins_size })
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
        let bin_size = u32_at_unchecked(bytes, bin + 8) as usize;
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
pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    cells(bytes)?;
    Ok(())
}

/// A key node. Field offsets below are from the cell start, so the signature is
/// at `at + 4` and everything else follows the documented `nk` layout.
#[derive(Clone, Copy)]
pub(crate) struct Key {
    pub at: usize,
}

// Every accessor below reads a field out of the file rather than out of
// something already validated, so every one of them is bounds-checked and
// returns `Err` instead of panicking. `self.at` itself can be garbage — it
// usually comes from an offset some other key stored on disk — so even a
// read of `self.at`'s own well-known fields must not assume it lands
// in-bounds.
impl Key {
    pub fn subkey_count(&self, b: &[u8]) -> Result<u32> {
        checked_u32(b, self.at + 24)
    }
    pub fn subkey_list(&self, b: &[u8]) -> Result<u32> {
        checked_u32(b, self.at + 32)
    }
    pub fn value_count(&self, b: &[u8]) -> Result<u32> {
        checked_u32(b, self.at + 40)
    }
    pub fn value_list(&self, b: &[u8]) -> Result<u32> {
        checked_u32(b, self.at + 44)
    }
    pub fn security(&self, b: &[u8]) -> Result<u32> {
        checked_u32(b, self.at + 48)
    }
    /// The key's own last-written timestamp, which new children inherit so
    /// that nothing here ever reads a clock.
    pub fn timestamp(&self, b: &[u8]) -> Result<u64> {
        checked_u64(b, self.at + 8)
    }

    pub fn name(&self, b: &[u8]) -> Result<String> {
        let len = checked_u16(b, self.at + 76)? as usize;
        let flags = checked_u16(b, self.at + 6)?;
        let raw = get(b, self.at + 80, len)?;
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
        let list = self.subkey_list(b)?;
        if self.subkey_count(b)? == 0 || list == u32::MAX {
            return Ok(Vec::new());
        }
        let at = BASE + list as usize;
        let sig = get(b, at + 4, 2)?;
        if sig != b"lf" && sig != b"lh" {
            bail!(
                "subkey list at {at:#x} is {}, which this does not edit",
                String::from_utf8_lossy(sig)
            );
        }
        let count = checked_u16(b, at + 6)? as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let off = checked_u32(b, at + 8 + i * 8)? as usize;
            let key = Key { at: BASE + off };
            out.push((key.name(b)?, key));
        }
        Ok(out)
    }

    pub fn value(&self, b: &[u8], name: &str) -> Result<Option<Vec<u8>>> {
        let count = self.value_count(b)? as usize;
        if count == 0 {
            return Ok(None);
        }
        let list = BASE + self.value_list(b)? as usize;
        for i in 0..count {
            let vk = BASE + checked_u32(b, list + 4 + i * 4)? as usize;
            let nlen = checked_u16(b, vk + 6)? as usize;
            let vk_flags = checked_u16(b, vk + 20)?;
            let raw_name = get(b, vk + 24, nlen)?;
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
            let raw = checked_u32(b, vk + 8)?;
            let len = (raw & 0x7fff_ffff) as usize;
            if raw & 0x8000_0000 != 0 {
                // Four bytes or fewer live in the offset field itself.
                return Ok(Some(get(b, vk + 12, len.min(4))?.to_vec()));
            }
            let data = BASE + checked_u32(b, vk + 12)? as usize + 4;
            return Ok(Some(get(b, data, len)?.to_vec()));
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
            // `bin_at` is where the bins region ends per the base block's
            // own `bins_size`, which is what `cells()` also trusts. On
            // every store this module has read, the file's length is
            // exactly `bin_at` (base block + bins, nothing after), so this
            // truncate is a no-op. It is checked, not assumed: a file with
            // trailing bytes past the declared bins region would have them
            // silently dropped, and this module's rule is to decline
            // rather than lose bytes silently.
            if bytes.len() != bin_at {
                bail!(
                    "hive has {} bytes past the declared bins region \
                     ({bin_at:#x}); refusing to drop them",
                    bytes.len() - bin_at
                );
            }
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

pub(crate) fn root(b: &[u8]) -> Result<Key> {
    Ok(Key { at: BASE + base_block(b)?.root_offset as usize })
}

/// Walks a path of subkey names from the root. `Ok(None)` means a name was not
/// found, which is a fact about the hive rather than an error.
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

/// What `enable_ems` did.
pub(crate) enum Outcome {
    /// Edited, and the result has been validated.
    Patched(Vec<u8>),
    /// Both elements were already present with the data we would have
    /// written. Use the source bytes.
    AlreadySet,
    /// Nothing was changed, for the reason given. Use the source bytes.
    NotApplicable(&'static str),
}

/// The global EMS settings object, on every Windows BCD store.
const EMS_OBJECT: &str = "{0ce4991b-e6b3-4b16-b23c-5e0d9250e5d9}";
/// `BcdLibraryInteger_EmsPort` and `BcdLibraryInteger_EmsBaudRate`.
const EMS_PORT: &str = "15000022";
const EMS_BAUD: &str = "15000023";
/// `BcdLibraryBoolean_EmsEnabled` — every real store surveyed carries this
/// already, and without it the port/baud rate we add achieve nothing:
/// EMS itself is off.
const BOOTEMS: &str = "16000020";

/// Adds `emsport` and `emsbaudrate` to a BCD store's `{emssettings}` object.
///
/// Never fails in a way a caller must handle: anything unexpected returns
/// `NotApplicable` and the caller writes the source bytes through. That is
/// deliberate. Losing EMS costs serial visibility during Setup; a hive we
/// corrupted costs the whole image, and on a rack with no framebuffer the
/// operator sees a machine that does nothing at all.
pub(crate) fn enable_ems(store: &[u8], port: u64, baud: u64) -> Outcome {
    let path = ["Objects", EMS_OBJECT, "Elements"];
    let elements = match find(store, &path) {
        Ok(Some(key)) => key,
        Ok(None) => return Outcome::NotApplicable("no {emssettings} object"),
        Err(_) => return Outcome::NotApplicable("not a readable hive"),
    };

    let existing = match elements.subkeys(store) {
        Ok(kids) => kids,
        Err(_) => return Outcome::NotApplicable("unsupported subkey list"),
    };

    // EMS only actually comes up if `{emssettings}` also carries `bootems`.
    // Every real store surveyed has it, but nothing enforces that a store
    // must, so without it the port/baud elements we are about to add would
    // achieve nothing and reporting `Patched` would be false — the same
    // shape of misreporting this feature exists to fix.
    if !existing.iter().any(|(n, _)| n == BOOTEMS) {
        return Outcome::NotApplicable("{emssettings} has no bootems");
    }

    // Filter by name first: a name already present must never be added a
    // second time, since a duplicate name in a subkey list is exactly what
    // the kernel's binary search (and `CmCheckRegistry`) does not
    // tolerate. Only report `AlreadySet` once every wanted name is present
    // *and* holds the data we would have written — an element present
    // with different data is left alone rather than silently overwritten
    // or silently reported as done.
    let wanted = [(EMS_PORT, port), (EMS_BAUD, baud)];
    let mut to_add = Vec::new();
    for (name, value) in wanted {
        match existing.iter().find(|(n, _)| n == name) {
            Some((_, key)) => {
                let read = match key.value(store, "Element") {
                    Ok(v) => v,
                    Err(_) => {
                        return Outcome::NotApplicable(
                            "existing element could not be read",
                        );
                    }
                };
                if read != Some(value.to_le_bytes().to_vec()) {
                    return Outcome::NotApplicable(
                        "element present with unexpected data",
                    );
                }
            }
            None => to_add.push((name, value)),
        }
    }
    if to_add.is_empty() {
        return Outcome::AlreadySet;
    }

    let mut out = store.to_vec();
    match add_elements(&mut out, elements, &existing, &to_add) {
        Ok(()) => {}
        Err(_) => return Outcome::NotApplicable("could not be edited"),
    }
    // The one rule with no exceptions: a hive we cannot verify is never
    // written to the volume.
    if validate(&out).is_err() {
        return Outcome::NotApplicable("edit did not validate");
    }
    // `validate` only checks that cells tile their bins; it says nothing
    // about whether the leaf we just rebuilt is in an order the kernel's
    // binary search can find things in. Check that here, on our own output
    // only — never in `validate`, which also runs on third-party stores
    // this module never wrote, where a leaf we judged unsorted might
    // simply use a comparison we have not modelled. A mistake here must
    // become `NotApplicable`, not a hive that validates yet mis-reads in
    // Windows.
    let rebuilt = match find(&out, &path) {
        Ok(Some(key)) => match key.subkeys(&out) {
            Ok(kids) => kids,
            Err(_) => {
                return Outcome::NotApplicable(
                    "rebuilt subkey list unreadable",
                );
            }
        },
        _ => return Outcome::NotApplicable("rebuilt {emssettings} missing"),
    };
    let kernel_sorted = rebuilt
        .windows(2)
        .all(|w| w[0].0.to_ascii_uppercase() <= w[1].0.to_ascii_uppercase());
    if !kernel_sorted {
        return Outcome::NotApplicable(
            "rebuilt subkey list is not sorted in kernel order",
        );
    }
    for &(name, want) in &to_add {
        let read = find(&out, &["Objects", EMS_OBJECT, "Elements", name])
            .ok()
            .flatten()
            .and_then(|k| k.value(&out, "Element").ok().flatten());
        if read != Some(want.to_le_bytes().to_vec()) {
            return Outcome::NotApplicable("edit did not read back");
        }
    }
    Outcome::Patched(out)
}

/// Writes one `nk` per element, each with a one-value list holding an
/// `Element` value, then rebuilds the parent's `lf` leaf with everything in
/// name order.
fn add_elements(
    out: &mut Vec<u8>,
    parent: Key,
    existing: &[(String, Key)],
    add: &[(&str, u64)],
) -> Result<()> {
    let sk = parent.security(out)?;
    let stamp = parent.timestamp(out)?;
    let mut kids: Vec<(String, u32)> = existing
        .iter()
        .map(|(n, k)| (n.clone(), (k.at - BASE) as u32))
        .collect();

    for (name, value) in add {
        let data_at = alloc(out, 8)?;
        let data = BASE + data_at as usize + 4;
        out[data..data + 8].copy_from_slice(&value.to_le_bytes());

        let vk_at = alloc(out, 20 + b"Element".len())?;
        let vk = BASE + vk_at as usize + 4;
        out[vk..vk + 2].copy_from_slice(b"vk");
        out[vk + 2..vk + 4].copy_from_slice(&7u16.to_le_bytes());
        out[vk + 4..vk + 8].copy_from_slice(&8u32.to_le_bytes());
        out[vk + 8..vk + 12].copy_from_slice(&data_at.to_le_bytes());
        out[vk + 12..vk + 16].copy_from_slice(&3u32.to_le_bytes()); // BINARY
        out[vk + 16..vk + 18].copy_from_slice(&1u16.to_le_bytes()); // ASCII
        out[vk + 20..vk + 27].copy_from_slice(b"Element");

        let list_at = alloc(out, 4)?;
        let list = BASE + list_at as usize + 4;
        out[list..list + 4].copy_from_slice(&vk_at.to_le_bytes());

        let nk_at = alloc(out, 76 + name.len())?;
        let nk = BASE + nk_at as usize + 4;
        out[nk..nk + 2].copy_from_slice(b"nk");
        out[nk + 2..nk + 4].copy_from_slice(&0x0020u16.to_le_bytes());
        out[nk + 4..nk + 12].copy_from_slice(&stamp.to_le_bytes());
        out[nk + 16..nk + 20]
            .copy_from_slice(&((parent.at - BASE) as u32).to_le_bytes());
        out[nk + 28..nk + 32].copy_from_slice(&u32::MAX.to_le_bytes());
        out[nk + 32..nk + 36].copy_from_slice(&u32::MAX.to_le_bytes());
        out[nk + 36..nk + 40].copy_from_slice(&1u32.to_le_bytes());
        out[nk + 40..nk + 44].copy_from_slice(&list_at.to_le_bytes());
        out[nk + 44..nk + 48].copy_from_slice(&sk.to_le_bytes());
        out[nk + 48..nk + 52].copy_from_slice(&u32::MAX.to_le_bytes());
        out[nk + 72..nk + 74]
            .copy_from_slice(&(name.len() as u16).to_le_bytes());
        out[nk + 76..nk + 76 + name.len()].copy_from_slice(name.as_bytes());

        kids.push((name.to_string(), nk_at));
    }

    // Security descriptors are reference counted, and `add.len()` more
    // keys now point at this one. `sk` came straight off the disk via
    // `parent.security`, so it is checked here before anything is written
    // through it: an out-of-range offset must not panic, and an in-range
    // offset that does not point at an `sk` cell must not have four
    // arbitrary bytes of some unrelated cell overwritten. Both are the
    // kind of corruption that would pass `validate` (which only checks bin
    // tiling) and the read-back check (which only checks the two new
    // elements), so this is the one place that check has to live.
    let sk_at = BASE + sk as usize;
    if sk_at + 20 > out.len() || &out[sk_at + 4..sk_at + 6] != b"sk" {
        bail!("security offset {sk:#x} does not name a security cell");
    }
    let refs = checked_u32(out, sk_at + 16)?.saturating_add(add.len() as u32);
    out[sk_at + 16..sk_at + 20].copy_from_slice(&refs.to_le_bytes());

    // A fresh leaf rather than growing the old one in place: the old cell
    // is left allocated but unreferenced, which is legal and costs 16
    // bytes.
    //
    // Sorted by the kernel's own comparison, not Rust's: the kernel
    // binary-searches an `lf`/`lh` leaf by upcasing each name before
    // comparing, so a leaf that is sorted under `Ord` but not under that
    // upcased rule reads as corrupt to Windows even though `validate` (bin
    // tiling only) sees nothing wrong. For the all-digit names this module
    // ever adds, byte order and upcased order coincide, so this is latent
    // today; it stops being latent the moment a store mixes cases (e.g.
    // `1600000F` next to `1600000a`).
    kids.sort_by(|a, b| {
        a.0.to_ascii_uppercase().cmp(&b.0.to_ascii_uppercase())
    });
    let leaf_at = alloc(out, 4 + kids.len() * 8)?;
    let leaf = BASE + leaf_at as usize + 4;
    // Always an `lf` leaf, even if the parent's existing subkey list was
    // `lh` (which additionally hashes each name for a faster compare). An
    // `lh` parent converting to `lf` is legal — the kernel accepts either
    // — and this module has never observed one in an eight-ISO survey, so
    // there is nothing to preserve in practice; the point is only that a
    // future `lh` store gets converted rather than misread.
    out[leaf..leaf + 2].copy_from_slice(b"lf");
    out[leaf + 2..leaf + 4].copy_from_slice(&(kids.len() as u16).to_le_bytes());
    for (i, (name, at)) in kids.iter().enumerate() {
        let e = leaf + 4 + i * 8;
        out[e..e + 4].copy_from_slice(&at.to_le_bytes());
        let mut hint = [0u8; 4];
        for (j, c) in name.bytes().take(4).enumerate() {
            hint[j] = c;
        }
        out[e + 4..e + 8].copy_from_slice(&hint);
    }

    out[parent.at + 24..parent.at + 28]
        .copy_from_slice(&(kids.len() as u32).to_le_bytes());
    out[parent.at + 32..parent.at + 36].copy_from_slice(&leaf_at.to_le_bytes());
    // `parent.at + 52` holds the parent's cached "largest subkey name
    // length" hint, which is left untouched here. That is only correct
    // because every name this module ever adds (`15000022`, `15000023`)
    // is the same 8 characters as `16000020`, which is already present
    // and required by the bootems check above — so the hint the parent
    // already carries cannot go stale. A future caller adding a
    // differently-sized name would need to maintain this field too.

    let sum = checksum(out);
    out[508..512].copy_from_slice(&sum.to_le_bytes());
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

    fn elements_of(v: &[u8]) -> Vec<String> {
        find(v, &["Objects", fixture::EMS_GUID, "Elements"])
            .unwrap()
            .unwrap()
            .subkeys(v)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    #[test]
    fn adds_both_elements_with_the_right_data() {
        let before = fixture::bcd_like(512);
        let Outcome::Patched(after) = enable_ems(&before, 1, 115200) else {
            panic!("expected a patch");
        };
        validate(&after).unwrap();

        let port = find(
            &after,
            &["Objects", fixture::EMS_GUID, "Elements", "15000022"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            port.value(&after, "Element").unwrap(),
            Some(1u64.to_le_bytes().to_vec())
        );

        let baud = find(
            &after,
            &["Objects", fixture::EMS_GUID, "Elements", "15000023"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            baud.value(&after, "Element").unwrap(),
            Some(115200u64.to_le_bytes().to_vec())
        );
    }

    /// The kernel binary-searches subkey lists, so an unsorted leaf is a hive
    /// that reads as corrupt.
    #[test]
    fn the_subkey_list_stays_sorted() {
        let Outcome::Patched(after) =
            enable_ems(&fixture::bcd_like(512), 1, 115200)
        else {
            panic!("expected a patch");
        };
        assert_eq!(elements_of(&after), ["15000022", "15000023", "16000020"]);
    }

    /// R19: the kernel upcases each name before comparing, so a leaf sorted
    /// by Rust's plain `Ord` can disagree with the kernel's own order.
    /// `1600000F` and `1600000a` are exactly such a pair: byte order says
    /// `F` (0x46) sorts before `a` (0x61), but upcased order says `1600000A`
    /// sorts before `1600000F` since `A` (0x41) precedes `F` (0x46). A fix
    /// that only changed byte order (or forgot to upcase) would still pass
    /// `the_subkey_list_stays_sorted` above, since that fixture's names are
    /// all-digit and cannot tell the two rules apart.
    #[test]
    fn rebuilds_the_leaf_in_kernel_collation_order() {
        let mut store = fixture::bcd_like(512);
        let elements =
            find(&store, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        let existing = elements.subkeys(&store).unwrap();
        add_elements(
            &mut store,
            elements,
            &existing,
            &[("1600000F", 1), ("1600000a", 2)],
        )
        .unwrap();
        validate(&store).unwrap();

        let Outcome::Patched(after) = enable_ems(&store, 1, 115200) else {
            panic!("expected a patch");
        };
        validate(&after).unwrap();
        // Kernel collation upcases before comparing: "1600000a" and
        // "1600000F" share a prefix with "16000020" only through
        // "160000", and diverge at the next character — '0' (from both
        // mixed-case names) sorts before '2' (from "16000020") — so both
        // mixed-case names precede "16000020" here, which byte order alone
        // would not obviously predict either.
        assert_eq!(
            elements_of(&after),
            ["15000022", "15000023", "1600000a", "1600000F", "16000020"]
        );
    }

    #[test]
    fn the_existing_bootems_element_survives() {
        let Outcome::Patched(after) =
            enable_ems(&fixture::bcd_like(512), 1, 115200)
        else {
            panic!("expected a patch");
        };
        let k = find(
            &after,
            &["Objects", fixture::EMS_GUID, "Elements", "16000020"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(k.value(&after, "Element").unwrap(), Some(vec![1]));
    }

    #[test]
    fn patching_twice_is_a_no_op() {
        let Outcome::Patched(once) =
            enable_ems(&fixture::bcd_like(512), 1, 115200)
        else {
            panic!("expected a patch");
        };
        assert!(matches!(enable_ems(&once, 1, 115200), Outcome::AlreadySet));
    }

    #[test]
    fn the_edit_is_deterministic() {
        let a = enable_ems(&fixture::bcd_like(512), 1, 115200);
        let b = enable_ems(&fixture::bcd_like(512), 1, 115200);
        match (a, b) {
            (Outcome::Patched(a), Outcome::Patched(b)) => assert_eq!(a, b),
            _ => panic!("expected two patches"),
        }
    }

    #[test]
    fn a_hive_with_no_free_space_grows_a_bin() {
        let before = fixture::bcd_like(8);
        let Outcome::Patched(after) = enable_ems(&before, 1, 115200) else {
            panic!("expected a patch");
        };
        assert!(after.len() > before.len());
        validate(&after).unwrap();
        assert_eq!(elements_of(&after), ["15000022", "15000023", "16000020"]);
    }

    #[test]
    fn declines_anything_that_is_not_a_hive() {
        assert!(matches!(
            enable_ems(b"this is not a hive at all", 1, 115200),
            Outcome::NotApplicable(_)
        ));
    }

    #[test]
    fn declines_a_hive_with_no_emssettings_object() {
        // The fixture without its Objects tree: a valid hive, wrong shape.
        let mut v = fixture::bcd_like(512);
        let root = root(&v).unwrap();
        v[root.at + 24..root.at + 28].copy_from_slice(&0u32.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(matches!(enable_ems(&v, 1, 115200), Outcome::NotApplicable(_)));
    }

    /// R18: EMS only actually comes up if `{emssettings}` also carries
    /// `bootems`. A store missing it must not be patched — the port and
    /// baud rate we would add achieve nothing without it, so writing them
    /// and reporting `Patched` would be false, the same way this whole
    /// feature exists because a false "success" was invisible.
    #[test]
    fn declines_when_bootems_is_missing() {
        let mut v = fixture::bcd_like(512);
        let elements = find(&v, &["Objects", fixture::EMS_GUID, "Elements"])
            .unwrap()
            .unwrap();
        // Empty the subkey list: bootems was the only child, so this
        // removes it and leaves nothing else either.
        v[elements.at + 24..elements.at + 28]
            .copy_from_slice(&0u32.to_le_bytes());
        v[elements.at + 32..elements.at + 36]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        let before = v.clone();
        match enable_ems(&v, 1, 115200) {
            Outcome::NotApplicable(why) => {
                assert_eq!(why, "{emssettings} has no bootems");
            }
            _ => panic!("expected NotApplicable"),
        }
        assert_eq!(v, before, "source bytes must be untouched");
    }

    /// A design decision — never clobber an element already present with
    /// data we did not write, such as someone's intentional COM2 — held up
    /// until now by nothing but the code reading that way.
    #[test]
    fn declines_when_port_is_already_set_to_something_else() {
        let before = fixture::bcd_like(512);
        let elements =
            find(&before, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        let existing = elements.subkeys(&before).unwrap();
        let mut seeded = before;
        add_elements(&mut seeded, elements, &existing, &[(EMS_PORT, 2)])
            .unwrap();
        validate(&seeded).unwrap();
        let snapshot = seeded.clone();
        match enable_ems(&seeded, 1, 115200) {
            Outcome::NotApplicable(why) => {
                assert_eq!(why, "element present with unexpected data");
            }
            _ => panic!("expected NotApplicable"),
        }
        assert_eq!(seeded, snapshot, "source bytes must be untouched");
    }

    /// R11: an offset read out of the file — here, the `Elements` key's own
    /// security offset — must never be trusted far enough to panic. Before
    /// the fix this reached `sk_at = BASE + 0xFFFFFFFF`, then indexed miles
    /// past the end of the buffer while incrementing the reference count,
    /// and unwound out of `oxwin-core` instead of returning
    /// `NotApplicable`.
    #[test]
    fn does_not_panic_on_a_garbage_security_offset() {
        let mut v = fixture::bcd_like(512);
        let elements = find(&v, &["Objects", fixture::EMS_GUID, "Elements"])
            .unwrap()
            .unwrap();
        v[elements.at + 48..elements.at + 52]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(matches!(enable_ems(&v, 1, 115200), Outcome::NotApplicable(_)));
    }

    /// R11: `Key::name` reads its length prefix from the file and, before
    /// the fix, sliced `self.at + 80 .. self.at + 80 + len` directly. A
    /// length of `0xffff` on a small fixture reads miles past the end of
    /// the buffer, which panicked with a slice-index error rather than
    /// declining. `elements.subkeys(store)` calls `name()` on every child
    /// it enumerates — including `16000020` here — so this reaches the
    /// same accessor `enable_ems` depends on throughout the walk.
    #[test]
    fn does_not_panic_on_a_garbage_name_length() {
        let mut v = fixture::bcd_like(512);
        let bootems =
            find(&v, &["Objects", fixture::EMS_GUID, "Elements", "16000020"])
                .unwrap()
                .unwrap();
        v[bootems.at + 76..bootems.at + 78]
            .copy_from_slice(&0xffffu16.to_le_bytes());
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(matches!(enable_ems(&v, 1, 115200), Outcome::NotApplicable(_)));
    }

    /// R12: a store with the port element already correct but no baud
    /// element must add only the baud element — no duplicate name, and
    /// the existing port element is left exactly as it was.
    #[test]
    fn adds_only_the_missing_baud_element() {
        let before = fixture::bcd_like(512);
        let elements =
            find(&before, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        let existing = elements.subkeys(&before).unwrap();
        let mut seeded = before.clone();
        add_elements(&mut seeded, elements, &existing, &[(EMS_PORT, 1)])
            .unwrap();
        validate(&seeded).unwrap();
        assert_eq!(elements_of(&seeded), ["15000022", "16000020"]);

        let Outcome::Patched(after) = enable_ems(&seeded, 1, 115200) else {
            panic!("expected a patch");
        };
        validate(&after).unwrap();
        assert_eq!(elements_of(&after), ["15000022", "15000023", "16000020"]);
        let elements_after =
            find(&after, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        assert_eq!(elements_after.subkey_count(&after).unwrap(), 3);
        let port = find(
            &after,
            &["Objects", fixture::EMS_GUID, "Elements", "15000022"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            port.value(&after, "Element").unwrap(),
            Some(1u64.to_le_bytes().to_vec())
        );
        let baud = find(
            &after,
            &["Objects", fixture::EMS_GUID, "Elements", "15000023"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            baud.value(&after, "Element").unwrap(),
            Some(115200u64.to_le_bytes().to_vec())
        );
    }

    /// R12, the exact defect: a store with the baud element already
    /// present but no port element must add only the port element. The
    /// original guard checked only `EMS_PORT`'s presence, so this shape
    /// missed entirely and the rebuilt leaf gained a second `15000023`
    /// entry with the same name — a duplicate the kernel's binary search
    /// does not tolerate.
    #[test]
    fn adds_only_the_missing_port_element() {
        let before = fixture::bcd_like(512);
        let elements =
            find(&before, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        let existing = elements.subkeys(&before).unwrap();
        let mut seeded = before.clone();
        add_elements(&mut seeded, elements, &existing, &[(EMS_BAUD, 115200)])
            .unwrap();
        validate(&seeded).unwrap();
        assert_eq!(elements_of(&seeded), ["15000023", "16000020"]);

        let Outcome::Patched(after) = enable_ems(&seeded, 1, 115200) else {
            panic!("expected a patch");
        };
        validate(&after).unwrap();
        assert_eq!(elements_of(&after), ["15000022", "15000023", "16000020"]);
        let elements_after =
            find(&after, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        assert_eq!(elements_after.subkey_count(&after).unwrap(), 3);
    }

    /// R12: with both elements present and matching, the result is
    /// `AlreadySet` — `patching_twice_is_a_no_op` above covers this state
    /// via a round trip; this test covers it directly against a
    /// hand-seeded store instead of a self-produced one.
    #[test]
    fn already_set_when_both_elements_match() {
        let before = fixture::bcd_like(512);
        let elements =
            find(&before, &["Objects", fixture::EMS_GUID, "Elements"])
                .unwrap()
                .unwrap();
        let existing = elements.subkeys(&before).unwrap();
        let mut seeded = before;
        add_elements(
            &mut seeded,
            elements,
            &existing,
            &[(EMS_PORT, 1), (EMS_BAUD, 115200)],
        )
        .unwrap();
        assert!(matches!(enable_ems(&seeded, 1, 115200), Outcome::AlreadySet));
    }

    /// A real BCD store's `Elements` list is an `lf` leaf; an `li`
    /// index-root is a shape this module declines rather than mis-reads.
    /// Silently doing nothing here would mean EMS never gets enabled with
    /// no reason surfaced, which Task 6 needs to see.
    #[test]
    fn declines_an_index_root_subkey_list() {
        let mut v = fixture::bcd_like(512);
        let elements = find(&v, &["Objects", fixture::EMS_GUID, "Elements"])
            .unwrap()
            .unwrap();
        let list_off = elements.subkey_list(&v).unwrap();
        let at = BASE + list_off as usize;
        v[at + 4..at + 6].copy_from_slice(b"li");
        let sum = checksum(&v);
        v[508..512].copy_from_slice(&sum.to_le_bytes());
        assert!(matches!(enable_ems(&v, 1, 115200), Outcome::NotApplicable(_)));
    }

    /// The editor against a real store, which is the only input that can
    /// disprove a shared misunderstanding between our fixture writer and our
    /// reader.
    ///
    ///   OXWIN_TEST_ISO=~/Desktop/…iso cargo test -p oxwin-core patches_a_real_bcd_store
    #[test]
    fn patches_a_real_bcd_store() {
        let Ok(iso) = std::env::var("OXWIN_TEST_ISO") else {
            eprintln!("skipped: set OXWIN_TEST_ISO");
            return;
        };
        let media = crate::media::Media::at(iso);
        let mut source =
            crate::media::Source::open(&media).expect("open the media");
        let file = source
            .find("/efi/microsoft/boot/bcd")
            .expect("media carries a UEFI BCD store");
        let store = source.read(&file).expect("read the store");

        let before = find(&store, &["Objects", EMS_OBJECT, "Elements"])
            .unwrap()
            .expect("stock media has an {emssettings} object")
            .subkeys(&store)
            .unwrap();
        assert!(
            before.iter().any(|(n, _)| n == "16000020"),
            "stock media should already have bootems"
        );
        assert!(
            !before.iter().any(|(n, _)| n == EMS_PORT),
            "stock media should not already have a port"
        );

        let Outcome::Patched(after) = enable_ems(&store, 1, 115200) else {
            panic!("a real store should patch");
        };
        validate(&after).unwrap();
        let port = find(&after, &["Objects", EMS_OBJECT, "Elements", EMS_PORT])
            .unwrap()
            .unwrap();
        assert_eq!(
            port.value(&after, "Element").unwrap(),
            Some(1u64.to_le_bytes().to_vec())
        );
    }

    fn golden_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/hive")
    }

    fn golden(name: &str) -> Vec<u8> {
        let path = golden_dir().join(name);
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!("{}: {e}. Regenerate with dump_goldens", path.display())
        })
    }

    #[test]
    fn matches_the_goldens_byte_for_byte() {
        assert_eq!(fixture::bcd_like(512), golden("bcd-like-before.bin"));
        assert_eq!(fixture::bcd_like(8), golden("bcd-like-full-before.bin"));

        let Outcome::Patched(after) =
            enable_ems(&fixture::bcd_like(512), 1, 115200)
        else {
            panic!("expected a patch");
        };
        assert_eq!(after, golden("bcd-like-after.bin"));

        let Outcome::Patched(after) =
            enable_ems(&fixture::bcd_like(8), 1, 115200)
        else {
            panic!("expected a patch");
        };
        assert_eq!(after, golden("bcd-like-full-after.bin"));
    }

    /// The comparison above is only evidence if it can fail. The trailing
    /// byte of the file sits in the leftover free-cell slack, so flipping
    /// it there would prove sensitivity at an offset nobody cares about.
    /// Flip a byte inside the new `15000022` key's own name instead, so
    /// the test proves the comparison is sensitive where the meaning
    /// lives.
    #[test]
    fn the_golden_comparison_can_fail() {
        let mut tampered = golden("bcd-like-after.bin");
        let port = find(
            &tampered,
            &["Objects", fixture::EMS_GUID, "Elements", "15000022"],
        )
        .unwrap()
        .unwrap();
        tampered[port.at + 80] ^= 0xff;
        let Outcome::Patched(after) =
            enable_ems(&fixture::bcd_like(512), 1, 115200)
        else {
            panic!("expected a patch");
        };
        assert_ne!(after, tampered);
    }

    ///   cargo test -p oxwin-core dump_goldens -- --ignored
    #[test]
    #[ignore = "rewrites goldens; run deliberately and read the diff"]
    fn dump_goldens() {
        let dir = golden_dir();
        std::fs::create_dir_all(&dir).expect("create testdata/hive");
        for (name, free) in [("bcd-like", 512usize), ("bcd-like-full", 8)] {
            let before = fixture::bcd_like(free);
            let Outcome::Patched(after) = enable_ems(&before, 1, 115200) else {
                panic!("expected a patch");
            };
            std::fs::write(dir.join(format!("{name}-before.bin")), &before)
                .expect("write before");
            std::fs::write(dir.join(format!("{name}-after.bin")), &after)
                .expect("write after");
        }
    }
}
