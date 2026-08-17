// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! A read-only UDF reader, enough to read a Windows installation ISO.
//!
//! Why this exists rather than a crate or a shell out to `hdiutil`:
//!
//! A Windows Server ISO is a UDF-bridge disc. Its ISO9660 layer is a stub — on the
//! Server 2022 media the entire ISO9660 root directory contains one file,
//! `README.TXT`. Everything real, including the 4.04 GiB `sources/install.wim`, lives
//! only in the UDF filesystem. That file is also larger than ISO9660's 4 GiB per-file
//! ceiling, so it could not be represented there anyway. Every ISO9660-only Rust crate
//! is therefore useless for this job, and no maintained Rust crate reads UDF.
//!
//! Scope is deliberately narrow: read-only, physical (Type 1) partitions, the
//! structures a mastered ISO actually uses. Anything outside that is a clear error
//! rather than a guess — a wrong guess here means a corrupt install image, which is
//! expensive to diagnose later.
//!
//! Reference: ECMA-167 3rd edition, and UDF 2.60. Field offsets below are from
//! ECMA-167 and are quoted in comments because they are impossible to review otherwise.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Sector size used for the anchor point search. UDF on optical media always uses
/// 2048, independent of the logical block size declared later.
const SECTOR: u64 = 2048;
const ANCHOR_LBA: u64 = 256;

// Descriptor tag identifiers we care about.
const TAG_PRIMARY_VOLUME: u16 = 1;
const TAG_ANCHOR: u16 = 2;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATING: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_IDENTIFIER: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

const FILE_TYPE_DIRECTORY: u8 = 4;

/// A location and length within the logical volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongAd {
    pub length: u32,
    pub block: u32,
    pub partition: u16,
}

#[derive(Debug, Clone, Copy)]
struct Extent {
    /// Logical block within the partition.
    block: u32,
    /// Bytes. Always a recorded extent by the time it reaches here.
    length: u32,
}

/// One file or directory found in the image.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Path with `/` separators, relative to the volume root, no leading slash.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    icb: LongAd,
}

pub struct UdfImage {
    file: File,
    logical_block_size: u32,
    /// Physical block where the partition begins; logical blocks are relative to it.
    partition_start: u32,
    partition_number: u16,
    root_icb: LongAd,
    label: String,
}

impl UdfImage {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;

        // The anchor names the extent holding the volume descriptor sequence. Try the
        // standard location first, then the tail of the image, which is where a
        // truncated or oddly mastered disc keeps its spare copy.
        let len = file.seek(SeekFrom::End(0))?;
        let mut anchor = None;
        let mut candidates = vec![ANCHOR_LBA];
        if len >= SECTOR * (ANCHOR_LBA + 1) {
            candidates.push(len / SECTOR - 1 - ANCHOR_LBA);
            candidates.push(len / SECTOR - 1);
        }
        for lba in candidates {
            if let Ok(buf) = read_at(&mut file, lba * SECTOR, 512) {
                if tag_id(&buf) == Some(TAG_ANCHOR) {
                    // AVDP: tag(16), MainVolumeDescriptorSequenceExtent(8) at 16.
                    let length = u32le(&buf, 16);
                    let location = u32le(&buf, 20);
                    if length > 0 {
                        anchor = Some((location, length));
                        break;
                    }
                }
            }
        }
        let (vds_block, vds_len) = anchor.context(
            "no UDF anchor volume descriptor pointer found — this does not look like a \
             UDF image, so it is probably not Windows installation media",
        )?;

        // Walk the volume descriptor sequence collecting the three things we need.
        let mut logical_block_size = 0u32;
        let mut fsd: Option<LongAd> = None;
        let mut partition: Option<(u16, u32)> = None;
        let mut label = String::new();

        let count = (vds_len as u64).div_ceil(SECTOR);
        for i in 0..count {
            let off = (vds_block as u64 + i) * SECTOR;
            let Ok(buf) = read_at(&mut file, off, SECTOR as usize) else {
                break;
            };
            match tag_id(&buf) {
                Some(TAG_TERMINATING) | None => break,
                Some(TAG_PRIMARY_VOLUME) => {
                    // PVD: VolumeIdentifier is a 32-byte dstring at offset 24.
                    label = dstring(&buf[24..56]);
                }
                Some(TAG_PARTITION) => {
                    // PD: PartitionNumber(2) at 22, PartitionStartingLocation(4) at 188.
                    partition = Some((u16le(&buf, 22), u32le(&buf, 188)));
                }
                Some(TAG_LOGICAL_VOLUME) => {
                    // LVD: LogicalBlockSize(4) at 212,
                    //      LogicalVolumeContentsUse(16) at 248 = long_ad of the File Set.
                    logical_block_size = u32le(&buf, 212);
                    fsd = Some(long_ad(&buf, 248));
                }
                _ => {}
            }
        }

        let (partition_number, partition_start) =
            partition.context("UDF image declares no partition descriptor")?;
        if logical_block_size == 0 {
            bail!("UDF image declares no logical volume descriptor");
        }
        // Everything downstream assumes byte offsets computed from this. A short read
        // would silently mis-address the whole volume.
        if logical_block_size as u64 != SECTOR {
            bail!(
                "UDF logical block size is {logical_block_size}, and only {SECTOR} is \
                 supported for ISO images"
            );
        }
        let fsd =
            fsd.context("UDF logical volume names no file set descriptor")?;

        let mut image = Self {
            file,
            logical_block_size,
            partition_start,
            partition_number,
            root_icb: LongAd { length: 0, block: 0, partition: 0 },
            label,
        };

        // The File Set Descriptor names the root directory's ICB.
        let fsd_bytes = image.read_extent_bytes(&[Extent {
            block: fsd.block,
            length: fsd.length.max(SECTOR as u32),
        }])?;
        if tag_id(&fsd_bytes) != Some(TAG_FILE_SET) {
            bail!("UDF file set descriptor is missing or malformed");
        }
        // FSD: RootDirectoryICB(long_ad) at 400.
        image.root_icb = long_ad(&fsd_bytes, 400);
        Ok(image)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Every file and directory in the image, depth first, paths using `/`.
    pub fn walk(&mut self) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let root = self.root_icb;
        self.walk_dir(root, "", &mut out, 0)?;
        Ok(out)
    }

    fn walk_dir(
        &mut self,
        icb: LongAd,
        prefix: &str,
        out: &mut Vec<Entry>,
        depth: usize,
    ) -> Result<()> {
        // Windows media nests a handful of levels. A limit costs nothing and turns a
        // corrupt image with a cyclic directory into an error instead of a hang.
        if depth > 32 {
            bail!("UDF directory nesting deeper than 32 levels at {prefix:?}");
        }
        for (name, characteristics, child) in self.read_dir(icb)? {
            let is_dir = characteristics & 0x02 != 0;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let size = if is_dir { 0 } else { self.file_size(child)? };
            out.push(Entry { path: path.clone(), size, is_dir, icb: child });
            if is_dir {
                self.walk_dir(child, &path, out, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Entries of one directory as (name, file characteristics, child ICB).
    fn read_dir(&mut self, icb: LongAd) -> Result<Vec<(String, u8, LongAd)>> {
        let data = self.read_icb_data(icb)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 38 <= data.len() {
            if tag_id(&data[i..]) != Some(TAG_FILE_IDENTIFIER) {
                // Directory extents are zero-padded to the block size; the first
                // non-descriptor is the end of the useful data.
                break;
            }
            // FID: FileCharacteristics(1) at 18, LengthOfFileIdentifier(1) at 19,
            //      ICB(long_ad) at 20, LengthOfImplementationUse(2) at 36.
            let characteristics = data[i + 18];
            let name_len = data[i + 19] as usize;
            let child = long_ad(&data[i..], 20);
            let impl_use = u16le(&data[i..], 36) as usize;
            let name_off = i + 38 + impl_use;
            if name_off + name_len > data.len() {
                bail!(
                    "UDF file identifier descriptor runs past the end of its extent"
                );
            }
            let total = 38 + impl_use + name_len;
            // Records are padded to a 4-byte boundary.
            let padded = (total + 3) & !3;

            // Bit 3 marks the parent pointer, which has no name and is not an entry.
            if characteristics & 0x08 == 0 && name_len > 0 {
                let name = d_characters(&data[name_off..name_off + name_len])?;
                // Bit 2 marks a deleted entry.
                if characteristics & 0x04 == 0 {
                    out.push((name, characteristics, child));
                }
            }
            i += padded;
        }
        Ok(out)
    }

    fn file_size(&mut self, icb: LongAd) -> Result<u64> {
        let fe = self.read_file_entry(icb)?;
        Ok(fe.information_length)
    }

    /// Copy a file's contents to a writer, streaming. Returns bytes written.
    ///
    /// `progress` is called with the running total, which is what makes a 4 GiB copy
    /// visible in the UI instead of a frozen window.
    pub fn copy_file(
        &mut self,
        entry: &Entry,
        out: &mut impl Write,
        mut progress: impl FnMut(u64),
    ) -> Result<u64> {
        if entry.is_dir {
            bail!("{} is a directory", entry.path);
        }
        let fe = self.read_file_entry(entry.icb)?;
        let mut written = 0u64;

        if let Some(embedded) = fe.embedded {
            out.write_all(&embedded)?;
            written = embedded.len() as u64;
            progress(written);
            return Ok(written);
        }

        let mut buf = vec![0u8; 8 * 1024 * 1024];
        let limit = fe.information_length;
        for e in fe.extents {
            let base = self.byte_offset(e.block);
            let mut done = 0u32;
            while done < e.length && written < limit {
                // All of this arithmetic is u64 on purpose. Narrowing the remaining
                // file length to u32 wraps for any file over 4 GiB, and when the wrap
                // lands exactly on zero the read below asks for nothing, makes no
                // progress, and spins forever. install.wim is 4.04 GiB.
                let want = ((e.length - done) as u64)
                    .min(buf.len() as u64)
                    .min(limit - written) as usize;
                if want == 0 {
                    break;
                }
                self.file.seek(SeekFrom::Start(base + done as u64))?;
                self.file.read_exact(&mut buf[..want]).with_context(|| {
                    format!(
                        "reading {} at extent block {}",
                        entry.path, e.block
                    )
                })?;
                out.write_all(&buf[..want])?;
                done += want as u32;
                written += want as u64;
                progress(written);
            }
        }

        if written != limit {
            bail!(
                "{}: expected {limit} bytes but the allocation descriptors held {written}",
                entry.path
            );
        }
        Ok(written)
    }

    /// Read `len` bytes from `offset` within a file.
    ///
    /// This is what makes the edition list cheap: the WIM's XML resource sits near the
    /// end of a 4.04 GiB file, and reading it is two of these rather than a copy.
    /// Returns fewer bytes than asked only at end of file.
    pub fn read_range(
        &mut self,
        entry: &Entry,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        if entry.is_dir {
            bail!("{} is a directory", entry.path);
        }
        let fe = self.read_file_entry(entry.icb)?;
        if let Some(embedded) = fe.embedded {
            let from = (offset as usize).min(embedded.len());
            let to = (from + len).min(embedded.len());
            return Ok(embedded[from..to].to_vec());
        }

        let limit = fe.information_length;
        let end = offset.saturating_add(len as u64).min(limit);
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        // Extents are consecutive stretches of the file, so walking them while
        // tracking the file-relative position is enough to find any offset.
        let mut position = 0u64;
        for e in &fe.extents {
            let extent_len = e.length as u64;
            let extent_end = position + extent_len;
            if extent_end > offset && position < end {
                let from = offset.max(position) - position;
                let to = end.min(extent_end) - position;
                self.file
                    .seek(SeekFrom::Start(self.byte_offset(e.block) + from))?;
                let mut chunk = vec![0u8; (to - from) as usize];
                self.file.read_exact(&mut chunk).with_context(|| {
                    format!(
                        "reading {} at extent block {}",
                        entry.path, e.block
                    )
                })?;
                out.extend_from_slice(&chunk);
            }
            position = extent_end;
            if position >= end {
                break;
            }
        }
        Ok(out)
    }

    /// Read a whole file into memory. Only for the small ones — the media files that
    /// get rewritten, not `install.wim`.
    pub fn read_file(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        let mut v = Vec::with_capacity(entry.size as usize);
        self.copy_file(entry, &mut v, |_| {})?;
        Ok(v)
    }

    /// The data an ICB points at, concatenated across extents.
    fn read_icb_data(&mut self, icb: LongAd) -> Result<Vec<u8>> {
        let fe = self.read_file_entry(icb)?;
        if let Some(embedded) = fe.embedded {
            return Ok(embedded);
        }
        self.read_extent_bytes(&fe.extents)
    }

    fn read_file_entry(&mut self, icb: LongAd) -> Result<FileEntry> {
        let off = self.byte_offset(icb.block);
        let size = icb.length.max(SECTOR as u32) as usize;
        let buf = read_at(&mut self.file, off, size)?;
        let tag = tag_id(&buf);
        let extended = match tag {
            Some(TAG_FILE_ENTRY) => false,
            Some(TAG_EXTENDED_FILE_ENTRY) => true,
            other => bail!(
                "expected a UDF file entry at block {}, found tag {:?}",
                icb.block,
                other
            ),
        };

        // ICBTag sits at 16; within it FileType is at +11 and Flags at +18.
        let file_type = buf[16 + 11];
        let icb_flags = u16le(&buf, 16 + 18);
        let ad_kind = icb_flags & 0x07;

        // Layout differs only in where the two lengths and the payload start.
        let (info_len, l_ea_off, l_ad_off, payload_off) = if extended {
            (u64le(&buf, 56), 208, 212, 216)
        } else {
            (u64le(&buf, 56), 168, 172, 176)
        };
        let l_ea = u32le(&buf, l_ea_off) as usize;
        let l_ad = u32le(&buf, l_ad_off) as usize;
        let ad_start = payload_off + l_ea;
        if ad_start + l_ad > buf.len() {
            bail!(
                "UDF file entry allocation descriptors run past the descriptor"
            );
        }

        // Type 3: the data is stored inside the file entry itself.
        if ad_kind == 3 {
            let end = ad_start + l_ad.min(info_len as usize);
            return Ok(FileEntry {
                information_length: info_len,
                is_dir: file_type == FILE_TYPE_DIRECTORY,
                extents: Vec::new(),
                embedded: Some(buf[ad_start..end].to_vec()),
            });
        }

        let extents = self.parse_allocation_descriptors(
            &buf[ad_start..ad_start + l_ad],
            ad_kind,
            icb.partition,
        )?;
        Ok(FileEntry {
            information_length: info_len,
            is_dir: file_type == FILE_TYPE_DIRECTORY,
            extents,
            embedded: None,
        })
    }

    /// Decode short_ad or long_ad lists, following continuation extents.
    fn parse_allocation_descriptors(
        &mut self,
        bytes: &[u8],
        ad_kind: u16,
        partition: u16,
    ) -> Result<Vec<Extent>> {
        // Owned, because following a continuation replaces the buffer we are scanning.
        let mut current = bytes.to_vec();
        let mut out = Vec::new();
        let step = match ad_kind {
            0 => 8,  // short_ad
            1 => 16, // long_ad
            other => {
                bail!("unsupported UDF allocation descriptor type {other}")
            }
        };

        // Bounded: a malformed image must not loop forever chasing continuations.
        for _ in 0..64 {
            let mut continuation: Option<Extent> = None;
            let mut i = 0usize;
            while i + step <= current.len() {
                let raw_len = u32le(&current, i);
                let length = raw_len & 0x3fff_ffff;
                let kind = raw_len >> 30;
                // Both short_ad and long_ad carry the block number at +4; long_ad
                // merely adds a partition reference after it.
                let block = u32le(&current, i + 4);
                if step == 16 {
                    let part = u16le(&current, i + 8);
                    if part != partition && part != self.partition_number {
                        bail!(
                            "UDF long allocation descriptor references partition {part}, \
                             but only one physical partition is supported"
                        );
                    }
                }
                if length == 0 {
                    break;
                }
                match kind {
                    // 0 = recorded and allocated. 1 and 2 are holes: not recorded, so
                    // they contribute zeros. Windows media does not use them, and
                    // silently treating one as data would corrupt the copy.
                    0 => out.push(Extent { block, length }),
                    1 | 2 => bail!("UDF sparse extents are not supported"),
                    3 => {
                        continuation = Some(Extent { block, length });
                        break;
                    }
                    _ => unreachable!("two bits"),
                }
                i += step;
            }
            match continuation {
                None => return Ok(out),
                Some(next) => {
                    // The continuation extent begins with its own Allocation Extent
                    // Descriptor: tag(16), PreviousAllocationExtentLocation(4) at 16,
                    // LengthOfAllocationDescriptors(4) at 20, then the descriptors.
                    let off = self.byte_offset(next.block);
                    let buf =
                        read_at(&mut self.file, off, next.length as usize)?;
                    let l_ad = u32le(&buf, 20) as usize;
                    let start = 24;
                    if start + l_ad > buf.len() {
                        bail!("UDF allocation extent descriptor is truncated");
                    }
                    current = buf[start..start + l_ad].to_vec();
                }
            }
        }
        bail!("UDF allocation descriptor chain is longer than 64 extents deep")
    }

    fn read_extent_bytes(&mut self, extents: &[Extent]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for e in extents {
            let off = self.byte_offset(e.block);
            let chunk = read_at(&mut self.file, off, e.length as usize)?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    fn byte_offset(&self, logical_block: u32) -> u64 {
        (self.partition_start as u64 + logical_block as u64)
            * self.logical_block_size as u64
    }
}

struct FileEntry {
    information_length: u64,
    #[allow(dead_code)]
    is_dir: bool,
    extents: Vec<Extent>,
    embedded: Option<Vec<u8>>,
}

// --- primitive decoding ---------------------------------------------------

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    if filled == 0 {
        bail!("read nothing at offset {offset}");
    }
    buf.truncate(filled);
    Ok(buf)
}

/// The tag identifier, but only if the tag's own checksum is right. Validating this
/// is what distinguishes "a descriptor lives here" from "these 16 bytes happen to
/// start with a small integer".
fn tag_id(buf: &[u8]) -> Option<u16> {
    if buf.len() < 16 {
        return None;
    }
    let sum = buf[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4 && *i != 5)
        .fold(0u8, |a, (_, b)| a.wrapping_add(*b));
    if sum != buf[4] {
        return None;
    }
    Some(u16le(buf, 0))
}

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64le(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

fn long_ad(b: &[u8], off: usize) -> LongAd {
    LongAd {
        length: u32le(b, off),
        block: u32le(b, off + 4),
        partition: u16le(b, off + 8),
    }
}

/// A UDF `dstring`: the last byte is the length, and byte 0 is the compression id.
fn dstring(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    let len = *b.last().unwrap() as usize;
    if len == 0 || len > b.len() - 1 {
        return String::new();
    }
    d_characters(&b[..len]).unwrap_or_default()
}

/// UDF file identifiers are OSTA compressed unicode: a leading byte says whether the
/// characters that follow are 8- or 16-bit.
fn d_characters(b: &[u8]) -> Result<String> {
    if b.is_empty() {
        return Ok(String::new());
    }
    match b[0] {
        8 => Ok(b[1..].iter().map(|c| *c as char).collect()),
        16 => {
            let mut units = Vec::with_capacity(b.len() / 2);
            let mut i = 1;
            while i + 1 < b.len() {
                units.push(u16::from_be_bytes([b[i], b[i + 1]]));
                i += 2;
            }
            Ok(String::from_utf16_lossy(&units))
        }
        // 0 means an empty identifier.
        0 => Ok(String::new()),
        other => bail!("unsupported UDF character encoding {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_checksum_rejects_random_bytes() {
        let mut buf = [0u8; 16];
        buf[0] = 5; // looks like a partition descriptor
        // Checksum byte left at zero, which is wrong for this content.
        assert_eq!(tag_id(&buf), None);
    }

    #[test]
    fn tag_checksum_accepts_a_well_formed_tag() {
        let mut buf = [0u8; 16];
        buf[0] = TAG_ANCHOR as u8;
        let sum = buf
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4 && *i != 5)
            .fold(0u8, |a, (_, b)| a.wrapping_add(*b));
        buf[4] = sum;
        assert_eq!(tag_id(&buf), Some(TAG_ANCHOR));
    }

    #[test]
    fn decodes_both_identifier_encodings() {
        // 8-bit: compression id 8 then Latin-1 bytes.
        assert_eq!(d_characters(&[8, b's', b'o', b'u', b'r']).unwrap(), "sour");
        // 16-bit: compression id 16 then UTF-16BE.
        assert_eq!(d_characters(&[16, 0, b'h', 0, b'i']).unwrap(), "hi");
        assert_eq!(d_characters(&[]).unwrap(), "");
    }

    #[test]
    fn rejects_an_unknown_encoding_rather_than_guessing() {
        assert!(d_characters(&[42, b'x']).is_err());
    }

    /// Runs only when `OXWIN_TEST_ISO` points at real Windows media, so CI without an
    /// ISO stays green. Worth having despite that: it is the only test that can catch
    /// the >4 GiB arithmetic bug, where narrowing the remaining length to u32 wrapped
    /// to exactly zero 45 MB into `install.wim` and span forever making no progress.
    #[test]
    fn reads_a_file_larger_than_four_gib() {
        let Some(iso) = std::env::var_os("OXWIN_TEST_ISO") else {
            eprintln!(
                "skipping: set OXWIN_TEST_ISO to a Windows ISO to run this"
            );
            return;
        };
        let mut img = UdfImage::open(Path::new(&iso)).expect("open");
        let entries = img.walk().expect("walk");
        let wim = entries
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case("sources/install.wim"))
            .expect("sources/install.wim");
        assert!(
            wim.size > 4 * 1024 * 1024 * 1024,
            "this ISO's install.wim is only {} bytes, so it cannot exercise the \
             overflow path this test exists for",
            wim.size
        );

        // Count bytes rather than keep them; the point is that the copy terminates
        // and reports exactly the declared length.
        struct Counter(u64);
        impl Write for Counter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0 += b.len() as u64;
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut c = Counter(0);
        let n = img.copy_file(wim, &mut c, |_| {}).expect("copy");
        assert_eq!(n, wim.size);
        assert_eq!(c.0, wim.size);
    }

    #[test]
    fn dstring_uses_its_trailing_length() {
        let mut field = [0u8; 32];
        field[0] = 8;
        field[1..5].copy_from_slice(b"TEST");
        field[31] = 5; // compression byte plus four characters
        assert_eq!(dstring(&field), "TEST");
    }
}
