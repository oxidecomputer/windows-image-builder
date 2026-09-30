// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! FAT32 volume construction.
//!
//! This builds the 64 MiB EFI System Partition the firmware actually boots: p2 of the
//! install image, carrying the UEFI Shell, the `startup.nsh` chooser and the UEFI:NTFS
//! loader with its exFAT driver. FAT is the only filesystem UEFI firmware is required
//! to read, which is why the big exFAT media volume needs this small partition.
//!
//! Clusters are handed out by a bump allocator, so every chain is contiguous. The image
//! is built once and never modified, so there is nothing to gain from a free-list, and
//! contiguous chains make both the FAT and the data placement trivial.
//!
//! Layout, for the 64 MiB / 512-byte-cluster case this produces:
//!
//! ```text
//!   sector 0        boot sector, then FSInfo at +1 and the backup pair at +6/+7
//!   sector 32       FAT #1 (1016 sectors), then FAT #2
//!   sector 2064     data region; cluster 2 is the root directory
//! ```
//!
//! Verified against p2 of an image that installed Windows on real Oxide hardware; see
//! `testdata/reference-esp.txt` and the tests at the bottom.

use crate::sparse::SparseImage;
use anyhow::{Result, bail};
use std::collections::HashSet;

/// One definition, shared with the partition table. A second copy of a constant like
/// this is exactly how the `OXIDE_UA`/`WINSETUP` volume-label drift happened.
const SECTOR: u64 = crate::mbr::SECTOR as u64;

/// Bytes per directory entry.
const ENTRY: usize = 32;

const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
/// Read-only | hidden | system | volume-id. A combination no real file has, which is
/// how a long-name entry hides from readers that predate long names.
const ATTR_LONG_NAME: u8 = 0x0f;

/// End of cluster chain.
const FAT_EOC: u32 = 0x0fff_ffff;

const LFN_CHARS_PER_ENTRY: usize = 13;

/// Where the 13 UTF-16 code units of a long-name entry live. Not contiguous, because
/// the fields at 11, 12, 13, 26 and 27 are shared with the short-entry layout so old
/// readers see a well-formed, uninteresting entry.
const LFN_SLOTS: [usize; LFN_CHARS_PER_ENTRY] =
    [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// FAT32 is only FAT32 with at least this many clusters. Below it, firmware reads the
/// volume as FAT16 and finds nothing, which is why the boot partition is 64 MiB and
/// not the 1 MiB it would otherwise need.
const MIN_FAT32_CLUSTERS: u32 = 65525;

/// A directory-entry timestamp, reduced to the two 16-bit fields FAT stores.
///
/// There is deliberately no time zone and no clock here. The builder has to be
/// reproducible, so a wall clock is not an option — and neither is a local-time
/// conversion. The builder this was ported from encoded an explicit
/// `Date.UTC(2020, 0, 1)` using local-time getters, so its output silently depended on
/// the time zone of whoever ran it; the reference image is stamped
/// `2019-12-31 19:00:00` for that reason. Taking the two encoded fields directly makes
/// that class of bug unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub date: u16,
    pub time: u16,
}

impl Timestamp {
    /// 2020-01-01 00:00:00 — what every image we build is stamped with.
    pub const MEDIA_EPOCH: Self =
        Self { date: (40 << 9) | (1 << 5) | 1, time: 0 };

    /// Encode a civil date and time. Seconds have two-second resolution on FAT, and
    /// an odd value rounds down, as the format requires.
    pub fn from_civil(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self> {
        if !(1980..=2107).contains(&year) {
            bail!("year {year} is outside FAT's 1980..=2107 range");
        }
        if !(1..=12).contains(&month) {
            bail!("month {month} is not 1..=12");
        }
        if !(1..=31).contains(&day) {
            bail!("day {day} is not 1..=31");
        }
        if hour > 23 || minute > 59 || second > 59 {
            bail!("time {hour}:{minute}:{second} is not a time of day");
        }
        Ok(Self {
            date: ((year - 1980) << 9) | ((month as u16) << 5) | (day as u16),
            time: ((hour as u16) << 11)
                | ((minute as u16) << 5)
                | ((second / 2) as u16),
        })
    }
}

/// How a file whose bytes were reserved rather than supplied is to be filled in.
///
/// The caller writes `size` bytes at `offset` in the finished image. This exists so a
/// 4 GiB `install.wim` can be streamed straight into its clusters instead of being
/// held in memory alongside the image that contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

/// Geometry derived from the volume's size, exposed because the orchestration needs to
/// know how much will fit before it starts adding files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub partition_sectors: u32,
    pub cluster_bytes: u64,
    pub sectors_per_fat: u32,
    pub cluster_count: u32,
    pub fat_start_sector: u64,
    pub data_start_sector: u64,
}

impl Geometry {
    /// Bytes available for file and directory contents.
    pub fn capacity_bytes(&self) -> u64 {
        self.cluster_count as u64 * self.cluster_bytes
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Total image size, including everything before `partition_start_lba`.
    pub size_bytes: u64,
    /// Volume label, at most 11 characters. Upper-cased.
    pub label: String,
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    /// Where the volume starts within the image. Zero produces a bare volume with no
    /// partition table, which is what an EFI System Partition written into a slot of
    /// somebody else's table needs.
    pub partition_start_lba: u32,
    /// 32-bit volume serial.
    pub volume_id: u32,
    pub stamp: Timestamp,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size_bytes: 1024 * 1024 * 1024,
            label: "OXIDE_UA".to_string(),
            sectors_per_cluster: 8,
            reserved_sectors: 32,
            partition_start_lba: 2048,
            volume_id: 0x01de_0000,
            stamp: Timestamp::MEDIA_EPOCH,
        }
    }
}

impl Options {
    /// The known-good configuration of p2: the EFI System Partition the firmware boots.
    ///
    /// 512-byte clusters because everything on it is small and a 64 MiB volume needs
    /// at least 65525 of them; `partition_start_lba` zero because it is written into a
    /// partition the caller's own table describes.
    pub fn esp(sectors: u32) -> Self {
        Self {
            size_bytes: sectors as u64 * SECTOR,
            label: "OXBOOT".to_string(),
            sectors_per_cluster: 1,
            partition_start_lba: 0,
            ..Self::default()
        }
    }
}

struct Node {
    name: String,
    is_dir: bool,
    /// Insertion-ordered, keyed by upper-cased name.
    ///
    /// The order is load-bearing twice over: it fixes both the order of directory
    /// entries and the order clusters are allocated in, so the image is only
    /// reproducible while it is stable. A hash map here would make the output differ
    /// run to run and quietly destroy the byte-compare gate.
    children: Vec<(String, usize)>,
    /// `None` for directories and for reserved files.
    data: Option<Vec<u8>>,
    reserved: bool,
    /// Absolute path, for `Placement`.
    path: String,
    parent: Option<usize>,
    short_name: [u8; 11],
    first_cluster: u32,
    cluster_count: u32,
    /// Directories report zero in their own entry, per the spec.
    byte_size: u64,
}

impl Node {
    fn new(name: String, is_dir: bool) -> Self {
        Self {
            name,
            is_dir,
            children: Vec::new(),
            data: None,
            reserved: false,
            path: String::new(),
            parent: None,
            short_name: [b' '; 11],
            first_cluster: 0,
            cluster_count: 0,
            byte_size: 0,
        }
    }
}

const ROOT: usize = 0;

pub struct Fat32Builder {
    label: String,
    sectors_per_cluster: u32,
    reserved_sectors: u32,
    partition_start_lba: u32,
    volume_id: u32,
    stamp: Timestamp,
    size_bytes: u64,
    geometry: Geometry,
    nodes: Vec<Node>,
    clusters_used: u32,
    placements: Vec<Placement>,
}

impl Fat32Builder {
    pub fn new(opts: Options) -> Result<Self> {
        let label = opts.label.to_uppercase();
        if label.chars().count() > 11 {
            bail!("volume label {label:?} exceeds 11 characters");
        }
        let geometry = geometry_for(&opts)?;
        Ok(Self {
            label,
            sectors_per_cluster: opts.sectors_per_cluster,
            reserved_sectors: opts.reserved_sectors,
            partition_start_lba: opts.partition_start_lba,
            volume_id: opts.volume_id,
            stamp: opts.stamp,
            size_bytes: opts.size_bytes,
            geometry,
            nodes: vec![Node::new(String::new(), true)],
            clusters_used: 0,
            placements: Vec::new(),
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Files whose bytes the caller still has to write. Empty until `build`.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Create `path` and every missing parent.
    pub fn add_dir(&mut self, path: &str) -> Result<usize> {
        let mut node = ROOT;
        for part in split_path(path) {
            let key = part.to_uppercase();
            match self.child(node, &key) {
                Some(existing) => {
                    if !self.nodes[existing].is_dir {
                        bail!(
                            "cannot create directory {path}: {part} is a file"
                        );
                    }
                    node = existing;
                }
                None => {
                    let mut child = Node::new(part.to_string(), true);
                    child.parent = Some(node);
                    let id = self.push(child);
                    self.nodes[node].children.push((key, id));
                    node = id;
                }
            }
        }
        Ok(node)
    }

    /// Add a file with its contents.
    pub fn add_file(&mut self, path: &str, data: Vec<u8>) -> Result<()> {
        let size = data.len() as u64;
        self.insert_file(path, size, Some(data), false)
    }

    /// Reserve `size` bytes for a file without supplying them. After `build`, the
    /// bytes go at the matching entry in `placements`.
    pub fn reserve_file(&mut self, path: &str, size: u64) -> Result<()> {
        self.insert_file(path, size, None, true)
    }

    fn insert_file(
        &mut self,
        path: &str,
        size: u64,
        data: Option<Vec<u8>>,
        reserved: bool,
    ) -> Result<()> {
        let mut parts = split_path(path);
        let name = match parts.pop() {
            Some(name) => name,
            None => bail!("a file needs a name, got {path:?}"),
        };
        if size > u32::MAX as u64 {
            bail!("{path} is {size} bytes, over FAT32's 4 GiB file limit");
        }
        let dir = if parts.is_empty() {
            ROOT
        } else {
            self.add_dir(&parts.join("/"))?
        };

        let mut node = Node::new(name.to_string(), false);
        node.data = data;
        node.reserved = reserved;
        node.byte_size = size;
        node.parent = Some(dir);
        node.path = if parts.is_empty() {
            format!("/{name}")
        } else {
            format!("/{}/{name}", parts.join("/"))
        };

        let key = name.to_uppercase();
        let id = self.push(node);
        // Last writer wins, in place, rather than erroring. Windows media already
        // ships `sources\EI.CFG` and a caller overriding it passes `sources/ei.cfg`;
        // on a case-insensitive filesystem those are one file, and two entries
        // differing only in case would be ambiguous to read back. Replacing in place
        // rather than appending keeps the entry order — and so the output — stable.
        match self.nodes[dir].children.iter().position(|(k, _)| *k == key) {
            Some(slot) => self.nodes[dir].children[slot] = (key, id),
            None => self.nodes[dir].children.push((key, id)),
        }
        Ok(())
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn child(&self, dir: usize, key: &str) -> Option<usize> {
        self.nodes[dir]
            .children
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, id)| *id)
    }

    fn kids(&self, dir: usize) -> Vec<usize> {
        self.nodes[dir].children.iter().map(|(_, id)| *id).collect()
    }

    pub fn build(&mut self) -> Result<SparseImage> {
        self.placements.clear();
        self.assign_short_names(ROOT);
        self.measure(ROOT, true)?;
        self.allocate()?;

        let mut image = SparseImage::new(self.size_bytes)?;
        self.write_mbr(&mut image)?;
        self.write_boot_sectors(&mut image)?;
        self.write_fats(&mut image)?;
        self.write_data(&mut image, ROOT, true)?;
        Ok(image)
    }

    // --- naming -----------------------------------------------------------

    fn assign_short_names(&mut self, dir: usize) {
        let mut taken: HashSet<[u8; 11]> = HashSet::new();
        for id in self.kids(dir) {
            let short = short_name_for(
                &self.nodes[id].name,
                self.nodes[id].is_dir,
                &taken,
            );
            taken.insert(short);
            self.nodes[id].short_name = short;
            if self.nodes[id].is_dir {
                self.assign_short_names(id);
            }
        }
    }

    // --- sizing -----------------------------------------------------------

    /// Directory sizes depend only on entry counts, so this runs before any cluster
    /// exists. That is what breaks the chicken and egg between a directory entry
    /// needing its child's first cluster and the child needing to be allocated.
    fn measure(&mut self, dir: usize, is_root: bool) -> Result<()> {
        // The volume label entry, or "." and "..".
        let mut entries: u64 = if is_root { 1 } else { 2 };
        for id in self.kids(dir) {
            entries += entries_for(&self.nodes[id].name) as u64;
            if self.nodes[id].is_dir {
                self.measure(id, false)?;
            } else {
                let clusters = self.nodes[id]
                    .byte_size
                    .div_ceil(self.geometry.cluster_bytes);
                self.nodes[id].cluster_count = clusters as u32;
            }
        }
        let bytes = entries * ENTRY as u64;
        let clusters = bytes.div_ceil(self.geometry.cluster_bytes).max(1);
        self.nodes[dir].cluster_count = clusters as u32;
        Ok(())
    }

    fn allocate(&mut self) -> Result<()> {
        // Cluster 2 is the root directory by convention; 0 and 1 are the FAT's own
        // media-descriptor entries and address nothing.
        let mut next: u32 = 2;
        self.claim(ROOT, &mut next)?;
        self.claim_children(ROOT, &mut next)?;

        self.clusters_used = next - 2;
        if self.clusters_used > self.geometry.cluster_count {
            bail!(
                "contents need {} but the volume holds {}",
                fmt_bytes(
                    self.clusters_used as u64 * self.geometry.cluster_bytes
                ),
                fmt_bytes(self.geometry.capacity_bytes()),
            );
        }
        Ok(())
    }

    fn claim(&mut self, id: usize, next: &mut u32) -> Result<()> {
        let count = self.nodes[id].cluster_count;
        // A zero-length file occupies no clusters and names cluster 0, which is how
        // FAT spells "no data at all".
        self.nodes[id].first_cluster = if count > 0 { *next } else { 0 };
        *next = next
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("cluster numbering overflowed"))?;
        Ok(())
    }

    fn claim_children(&mut self, dir: usize, next: &mut u32) -> Result<()> {
        for id in self.kids(dir) {
            self.claim(id, next)?;
            if self.nodes[id].is_dir {
                self.claim_children(id, next)?;
            }
        }
        Ok(())
    }

    // --- on-disk structures -----------------------------------------------

    /// A partition table describing the volume as the whole disk's only partition.
    ///
    /// With `partition_start_lba` zero the boot sector lands at offset 0 too and
    /// overwrites this, which is exactly what the EFI System Partition wants: it is
    /// one entry in a table the caller writes. Kept rather than special-cased so a
    /// standalone FAT32 image — the `--fs=fat32` path — still gets a table.
    fn write_mbr(&self, image: &mut SparseImage) -> Result<()> {
        let mut mbr = [0u8; SECTOR as usize];
        let p = 446;
        mbr[p] = 0x80; // bootable
        // CHS is vestigial for an LBA partition type; the conventional "too large to
        // express" sentinel beats computing geometry that would be fiction.
        mbr[p + 1..p + 4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        mbr[p + 4] = 0x0c; // FAT32 LBA
        mbr[p + 5..p + 8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        mbr[p + 8..p + 12]
            .copy_from_slice(&self.partition_start_lba.to_le_bytes());
        mbr[p + 12..p + 16]
            .copy_from_slice(&self.geometry.partition_sectors.to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xaa;
        image.write(0, &mbr)
    }

    fn write_boot_sectors(&self, image: &mut SparseImage) -> Result<()> {
        let mut boot = [0u8; SECTOR as usize];
        boot[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]); // jmp short +0x58; nop
        boot[3..11].copy_from_slice(b"MSWIN4.1");
        put16(&mut boot, 11, SECTOR as u16); // bytes per sector
        boot[13] = self.sectors_per_cluster as u8;
        put16(&mut boot, 14, self.reserved_sectors as u16);
        boot[16] = 2; // two FATs
        put16(&mut boot, 17, 0); // root entry count: zero on FAT32
        put16(&mut boot, 19, 0); // 16-bit total sectors: see offset 32
        boot[21] = 0xf8; // media descriptor: fixed disk
        put16(&mut boot, 22, 0); // 16-bit FAT size: zero on FAT32
        put16(&mut boot, 24, 63); // sectors per track
        put16(&mut boot, 26, 255); // heads
        put32(&mut boot, 28, self.partition_start_lba); // hidden sectors
        put32(&mut boot, 32, self.geometry.partition_sectors);
        put32(&mut boot, 36, self.geometry.sectors_per_fat);
        put16(&mut boot, 40, 0); // ext flags: FATs mirrored
        put16(&mut boot, 42, 0); // filesystem version
        put32(&mut boot, 44, 2); // root cluster
        put16(&mut boot, 48, 1); // FSInfo sector
        put16(&mut boot, 50, 6); // backup boot sector
        boot[64] = 0x80; // drive number
        boot[66] = 0x29; // extended boot signature
        put32(&mut boot, 67, self.volume_id);
        boot[71..82].copy_from_slice(&ascii_11(&self.label));
        boot[82..90].copy_from_slice(b"FAT32   ");
        boot[510] = 0x55;
        boot[511] = 0xaa;

        let mut fsinfo = [0u8; SECTOR as usize];
        put32(&mut fsinfo, 0, 0x4161_5252); // "RRaA"
        put32(&mut fsinfo, 484, 0x6141_7272); // "rrAa"
        put32(
            &mut fsinfo,
            488,
            self.geometry.cluster_count - self.clusters_used,
        );
        put32(&mut fsinfo, 492, self.clusters_used + 2); // next free hint
        put32(&mut fsinfo, 508, 0xaa55_0000);

        let base = self.partition_start_lba as u64 * SECTOR;
        image.write(base, &boot)?;
        image.write(base + SECTOR, &fsinfo)?;
        // The backup pair lives at partition-relative sectors 6 and 7. Firmware falls
        // back to it, so a volume with only the primary copy works right up until the
        // primary is damaged and then fails in a way nothing explains.
        image.write(base + 6 * SECTOR, &boot)?;
        image.write(base + 7 * SECTOR, &fsinfo)?;
        Ok(())
    }

    fn write_fats(&self, image: &mut SparseImage) -> Result<()> {
        // Only the head of the FAT is non-zero: a free cluster is zero, so the tail of
        // a mostly-empty volume needs no bytes written at all. On a 64 MiB volume that
        // is the difference between 9 KiB and 1 MiB of FAT.
        let entries = self.clusters_used as usize + 2;
        let mut fat = vec![0u8; entries * 4];
        put32(&mut fat, 0, 0x0fff_fff8); // media descriptor, entry 0
        put32(&mut fat, 4, FAT_EOC); // entry 1

        let chain = |node: &Node, fat: &mut [u8]| {
            for i in 0..node.cluster_count {
                let cluster = node.first_cluster + i;
                let next = if i == node.cluster_count - 1 {
                    FAT_EOC
                } else {
                    cluster + 1
                };
                put32(fat, cluster as usize * 4, next);
            }
        };
        chain(&self.nodes[ROOT], &mut fat);
        let mut stack = self.kids(ROOT);
        stack.reverse();
        while let Some(id) = stack.pop() {
            chain(&self.nodes[id], &mut fat);
            if self.nodes[id].is_dir {
                // Depth first, in insertion order, matching `allocate`. The chains
                // themselves do not care about order, but keeping the two walks
                // identical is what makes a mismatch here a loud test failure rather
                // than a subtly wrong FAT.
                let mut kids = self.kids(id);
                kids.reverse();
                stack.extend(kids);
            }
        }

        for index in 0..2u64 {
            let at = (self.geometry.fat_start_sector
                + index * self.geometry.sectors_per_fat as u64)
                * SECTOR;
            image.write(at, &fat)?;
        }
        Ok(())
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        (self.geometry.data_start_sector
            + (cluster as u64 - 2) * self.sectors_per_cluster as u64)
            * SECTOR
    }

    fn write_data(
        &mut self,
        image: &mut SparseImage,
        dir: usize,
        is_root: bool,
    ) -> Result<()> {
        let capacity =
            self.nodes[dir].cluster_count as u64 * self.geometry.cluster_bytes;
        let mut buf = vec![0u8; capacity as usize];
        let mut at = 0usize;
        let mut put = |entry: [u8; ENTRY], at: &mut usize| -> Result<()> {
            if *at + ENTRY > buf.len() {
                bail!(
                    "directory entries overflow the clusters measured for them"
                );
            }
            buf[*at..*at + ENTRY].copy_from_slice(&entry);
            *at += ENTRY;
            Ok(())
        };

        if is_root {
            put(self.volume_label_entry(), &mut at)?;
        } else {
            // Both names are space-padded to the full 11 bytes. Zero padding here
            // makes fsck read a three-byte extension of NULs and conclude this is not
            // a directory.
            let self_cluster = self.nodes[dir].first_cluster;
            put(self.dot_entry(b".          ", self_cluster), &mut at)?;
            // ".." names the root as cluster 0 rather than as cluster 2, per spec.
            let parent = self.nodes[dir].parent.unwrap_or(ROOT);
            let up = if parent == ROOT {
                0
            } else {
                self.nodes[parent].first_cluster
            };
            put(self.dot_entry(b"..         ", up), &mut at)?;
        }

        for id in self.kids(dir) {
            for lfn in self.lfn_entries(id) {
                put(lfn, &mut at)?;
            }
            put(self.short_entry(id), &mut at)?;
        }

        let dir_offset = self.cluster_offset(self.nodes[dir].first_cluster);
        image.write(dir_offset, &buf)?;

        for id in self.kids(dir) {
            if self.nodes[id].is_dir {
                self.write_data(image, id, false)?;
            } else if self.nodes[id].reserved {
                // The clusters are allocated and the directory entry is complete; the
                // caller streams the bytes into this offset afterwards.
                self.placements.push(Placement {
                    path: self.nodes[id].path.clone(),
                    offset: self.cluster_offset(self.nodes[id].first_cluster),
                    size: self.nodes[id].byte_size,
                });
            } else if let Some(data) = &self.nodes[id].data {
                if !data.is_empty() {
                    let offset =
                        self.cluster_offset(self.nodes[id].first_cluster);
                    // Cloning the slice reference is not possible while `image` is
                    // borrowed mutably alongside `self`, so take the offset first and
                    // write from the node's own buffer.
                    image.write(offset, data)?;
                }
            }
        }
        Ok(())
    }

    fn volume_label_entry(&self) -> [u8; ENTRY] {
        let mut e = [0u8; ENTRY];
        e[0..11].copy_from_slice(&ascii_11(&self.label));
        e[11] = ATTR_VOLUME_ID;
        self.stamp_times(&mut e);
        e
    }

    fn dot_entry(&self, name: &[u8; 11], cluster: u32) -> [u8; ENTRY] {
        let mut e = [0u8; ENTRY];
        e[0..11].copy_from_slice(name);
        e[11] = ATTR_DIRECTORY;
        put16(&mut e, 20, (cluster >> 16) as u16);
        put16(&mut e, 26, (cluster & 0xffff) as u16);
        self.stamp_times(&mut e);
        e
    }

    fn short_entry(&self, id: usize) -> [u8; ENTRY] {
        let node = &self.nodes[id];
        let mut e = [0u8; ENTRY];
        e[0..11].copy_from_slice(&node.short_name);
        e[11] = if node.is_dir { ATTR_DIRECTORY } else { ATTR_ARCHIVE };
        put16(&mut e, 20, (node.first_cluster >> 16) as u16);
        put16(&mut e, 26, (node.first_cluster & 0xffff) as u16);
        put32(&mut e, 28, if node.is_dir { 0 } else { node.byte_size as u32 });
        self.stamp_times(&mut e);
        e
    }

    fn stamp_times(&self, e: &mut [u8; ENTRY]) {
        put16(e, 14, self.stamp.time); // created
        put16(e, 16, self.stamp.date);
        put16(e, 18, self.stamp.date); // last accessed
        put16(e, 22, self.stamp.time); // last written
        put16(e, 24, self.stamp.date);
    }

    fn lfn_entries(&self, id: usize) -> Vec<[u8; ENTRY]> {
        let node = &self.nodes[id];
        if !needs_lfn(&node.name) {
            return Vec::new();
        }
        let checksum = short_name_checksum(&node.short_name);
        let chars: Vec<char> = node.name.chars().collect();
        let total = chars.len().div_ceil(LFN_CHARS_PER_ENTRY);
        let mut out = Vec::with_capacity(total);

        // Long-name entries are stored in reverse, last chunk first, with 0x40 marking
        // the one that comes first on disk as the last of the sequence.
        for seq in (1..=total).rev() {
            let mut e = [0u8; ENTRY];
            e[0] = if seq == total { seq as u8 | 0x40 } else { seq as u8 };
            e[11] = ATTR_LONG_NAME;
            e[13] = checksum;
            for i in 0..LFN_CHARS_PER_ENTRY {
                let index = (seq - 1) * LFN_CHARS_PER_ENTRY + i;
                let code = match index.cmp(&chars.len()) {
                    std::cmp::Ordering::Less => {
                        let c = chars[index] as u32;
                        // Anything outside the BMP needs a surrogate pair, which this
                        // one-code-unit-per-char loop cannot express. Nothing we put
                        // on the media is outside it; refusing beats truncating.
                        if c > 0xffff { 0xfffd } else { c as u16 }
                    }
                    std::cmp::Ordering::Equal => 0x0000, // terminator
                    std::cmp::Ordering::Greater => 0xffff, // pad
                };
                put16(&mut e, LFN_SLOTS[i], code);
            }
            out.push(e);
        }
        out
    }
}

fn geometry_for(opts: &Options) -> Result<Geometry> {
    if opts.sectors_per_cluster == 0 {
        bail!("sectors per cluster must be at least 1");
    }
    let total_sectors = opts.size_bytes / SECTOR;
    if total_sectors <= opts.partition_start_lba as u64 {
        bail!(
            "a {}-byte image has no room for a volume starting at LBA {}",
            opts.size_bytes,
            opts.partition_start_lba
        );
    }
    let partition_sectors =
        u32::try_from(total_sectors - opts.partition_start_lba as u64)
            .map_err(|_| anyhow::anyhow!("volume exceeds 2 TiB"))?;
    let cluster_bytes = opts.sectors_per_cluster as u64 * SECTOR;

    // The FAT sizing formula from Microsoft's FAT specification (fatgen103), FAT32
    // branch. The 256 is the spec's own constant rather than anything derivable; it
    // over-provisions slightly, which is the safe direction.
    let usable = partition_sectors
        .checked_sub(opts.reserved_sectors)
        .ok_or_else(|| anyhow::anyhow!("reserved sectors exceed the volume"))?;
    let divisor = (256 * opts.sectors_per_cluster + 2) / 2;
    let sectors_per_fat = usable.div_ceil(divisor);

    let data_sectors = usable
        .checked_sub(2 * sectors_per_fat)
        .ok_or_else(|| anyhow::anyhow!("the FATs do not fit in the volume"))?;
    let cluster_count = data_sectors / opts.sectors_per_cluster;
    if cluster_count < MIN_FAT32_CLUSTERS {
        bail!(
            "geometry yields {cluster_count} clusters; FAT32 requires at least \
             {MIN_FAT32_CLUSTERS}"
        );
    }

    let fat_start_sector =
        opts.partition_start_lba as u64 + opts.reserved_sectors as u64;
    Ok(Geometry {
        partition_sectors,
        cluster_bytes,
        sectors_per_fat,
        cluster_count,
        fat_start_sector,
        data_start_sector: fat_start_sector + 2 * sectors_per_fat as u64,
    })
}

// --- helpers ------------------------------------------------------------

fn put16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn split_path(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect()
}

/// Space-pad or truncate to the 11 bytes a label or short name occupies.
fn ascii_11(s: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    for (slot, c) in out.iter_mut().zip(s.chars()) {
        *slot = c as u32 as u8;
    }
    out
}

/// Characters legal in a short (8.3) name. Anything else becomes `_`.
fn short_name_ok(c: char) -> bool {
    c.is_ascii_uppercase()
        || c.is_ascii_digit()
        || "$%'-_@~`!(){}^#&".contains(c)
}

/// Split at the last dot, treating a leading dot as part of the name rather than as an
/// extension separator.
fn split_stem_ext(name: &str) -> (String, String) {
    let chars: Vec<char> = name.chars().collect();
    match chars.iter().rposition(|c| *c == '.').filter(|i| *i > 0) {
        Some(i) => {
            (chars[..i].iter().collect(), chars[i + 1..].iter().collect())
        }
        None => (name.to_string(), String::new()),
    }
}

fn needs_lfn(name: &str) -> bool {
    let (stem, ext) = split_stem_ext(name);
    if stem.is_empty()
        || stem.chars().count() > 8
        || ext.chars().count() > 3
        || name != name.to_uppercase()
        || name.chars().filter(|c| *c == '.').count() > 1
    {
        return true;
    }
    !stem.chars().chain(ext.chars()).all(short_name_ok)
}

fn short_name_for(
    name: &str,
    is_dir: bool,
    taken: &HashSet<[u8; 11]>,
) -> [u8; 11] {
    let (raw_stem, raw_ext) = split_stem_ext(name);
    // A directory has no extension, and the builder this was ported from dropped the
    // text after a directory's last dot rather than folding it into the stem, so
    // `Rufus.old` shortens to `RUFUS`. Preserved rather than corrected: no input of ours
    // reaches it, and changing it would change every volume's bytes — including the
    // goldens and every image already booted. See the test that pins the sharp edge.
    let raw_ext = if is_dir { String::new() } else { raw_ext };

    let clean = |s: &str| -> String {
        s.to_uppercase()
            .chars()
            .filter(|c| *c != ' ')
            .map(|c| if short_name_ok(c) { c } else { '_' })
            .collect()
    };
    let stem = {
        let cleaned = clean(&raw_stem);
        if cleaned.is_empty() { "_".to_string() } else { cleaned }
    };
    let ext: String = clean(&raw_ext).chars().take(3).collect();

    let mut candidate: String = stem.chars().take(8).collect();
    // On collision, or whenever truncation happened, fall back to the `~N` tail form.
    // Truncation alone is enough: two different long names can share their first
    // eight characters, and the tail is what keeps them distinct.
    if stem.chars().count() > 8 || taken.contains(&pad83(&candidate, &ext)) {
        for n in 1..1_000_000u32 {
            let tail = format!("~{n}");
            let keep = 8usize.saturating_sub(tail.len());
            candidate =
                stem.chars().take(keep).collect::<String>() + tail.as_str();
            if !taken.contains(&pad83(&candidate, &ext)) {
                break;
            }
        }
    }
    pad83(&candidate, &ext)
}

fn pad83(stem: &str, ext: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    for (slot, c) in out[0..8].iter_mut().zip(stem.chars()) {
        *slot = c as u32 as u8;
    }
    for (slot, c) in out[8..11].iter_mut().zip(ext.chars()) {
        *slot = c as u32 as u8;
    }
    out
}

/// The checksum every long-name entry carries, tying it to its short entry. A reader
/// that finds a mismatch discards the long name, so getting this wrong shows up as
/// files that appear under their 8.3 names only.
fn short_name_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for byte in short {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*byte);
    }
    sum
}

fn entries_for(name: &str) -> usize {
    let lfn = if needs_lfn(name) {
        name.chars().count().div_ceil(LFN_CHARS_PER_ENTRY)
    } else {
        0
    };
    lfn + 1
}

fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// p2 of `ws2022-win-server-01-install.img`: 64 MiB, 131072 sectors.
    const ESP_SECTORS: u32 = 131_072;

    /// Sizes of the four files on the reference volume, in the order they were added.
    /// The directory entries and the FAT depend on these and on nothing else about the
    /// payload, which is what lets these tests run without the gitignored downloads.
    const REFERENCE_FILES: &[(&str, u64)] = &[
        ("/EFI/BOOT/BOOTX64.EFI", 1_137_728),
        ("/EFI/BOOT/uefintfs.efi", 31_888),
        ("/EFI/Rufus/exfat_x64.efi", 41_728),
        ("/startup.nsh", 567),
    ];

    /// What `fat32.js` stamped the reference volume with: 2019-12-31 19:00:00, the
    /// US/Eastern rendering of the midnight UTC it was asked for. Spelled out here so
    /// the port is compared against what was really written, not against what the JS
    /// meant to write.
    fn reference_stamp() -> Timestamp {
        Timestamp::from_civil(2019, 12, 31, 19, 0, 0).unwrap()
    }

    fn reference_esp() -> Fat32Builder {
        let mut b = Fat32Builder::new(Options {
            stamp: reference_stamp(),
            ..Options::esp(ESP_SECTORS)
        })
        .unwrap();
        for (path, size) in REFERENCE_FILES {
            b.reserve_file(path, *size).unwrap();
        }
        b
    }

    /// Named golden regions of the reference volume, from `testdata/reference-esp.txt`.
    fn golden(name: &str) -> Vec<u8> {
        let text = include_str!("../testdata/reference-esp.txt");
        let hex = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix(name).map(str::trim))
            .unwrap_or_else(|| panic!("no golden region named {name}"));
        assert!(hex.len().is_multiple_of(2), "{name}: odd hex length");
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// Report where two byte strings first differ, so a failure names an offset
    /// instead of dumping two blobs.
    fn first_difference(a: &[u8], b: &[u8]) -> Option<String> {
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            if x != y {
                return Some(format!(
                    "byte {i} (0x{i:x}): expected 0x{x:02x}, got 0x{y:02x}"
                ));
            }
        }
        if a.len() != b.len() {
            return Some(format!("length {} vs {}", a.len(), b.len()));
        }
        None
    }

    fn assert_region(image: &SparseImage, offset: u64, name: &str) {
        let want = golden(name);
        let got = image.read(offset, want.len());
        if let Some(where_) = first_difference(&want, &got) {
            panic!("{name} differs from the reference volume: {where_}");
        }
    }

    #[test]
    fn geometry_matches_the_reference_volume() {
        let g = reference_esp().geometry();
        assert_eq!(g.partition_sectors, 131_072);
        assert_eq!(g.cluster_bytes, 512);
        assert_eq!(g.sectors_per_fat, 1016);
        assert_eq!(g.cluster_count, 129_008);
        assert_eq!(g.fat_start_sector, 32);
        assert_eq!(g.data_start_sector, 2064);
    }

    /// The boot sector and FSInfo are the first bytes firmware reads, and every
    /// geometry field lands in them. This is the single highest-value comparison in
    /// the file.
    #[test]
    fn boot_sector_and_fsinfo_match_the_reference_volume() {
        let image = reference_esp().build().unwrap();
        assert_region(&image, 0, "boot-sector");
        assert_region(&image, SECTOR, "fsinfo");
    }

    #[test]
    fn the_backup_pair_is_a_copy_at_sectors_six_and_seven() {
        let image = reference_esp().build().unwrap();
        assert_eq!(image.read(0, 512), image.read(6 * SECTOR, 512));
        assert_eq!(image.read(SECTOR, 512), image.read(7 * SECTOR, 512));
    }

    /// Directory entries carry the long names, the short names, the cluster numbers,
    /// the sizes and the timestamps — everything that makes a file findable.
    #[test]
    fn directory_clusters_match_the_reference_volume() {
        let builder = reference_esp();
        let image = reference_esp().build().unwrap();
        // Cluster numbers observed in the reference volume.
        for (cluster, name) in [
            (2, "dir-root"),
            (3, "dir-efi"),
            (4, "dir-boot"),
            (2291, "dir-rufus"),
        ] {
            assert_region(&image, builder.cluster_offset(cluster), name);
        }
    }

    #[test]
    fn the_fat_head_matches_and_is_mirrored() {
        let mut builder = reference_esp();
        let image = builder.build().unwrap();
        let g = builder.geometry();
        let fat1 = g.fat_start_sector * SECTOR;
        let fat2 = (g.fat_start_sector + g.sectors_per_fat as u64) * SECTOR;
        assert_region(&image, fat1, "fat-head");
        // Two FATs, mirrored, as the boot sector's zero ext-flags promises.
        let len = (builder.clusters_used as usize + 2) * 4;
        assert_eq!(image.read(fat1, len), image.read(fat2, len));
    }

    /// Cluster numbers read out of the reference volume. If the bump allocator ever
    /// walks the tree in a different order, every one of these moves.
    #[test]
    fn files_land_on_the_clusters_the_reference_volume_used() {
        let mut builder = reference_esp();
        builder.build().unwrap();
        let placed: Vec<(String, u64)> = builder
            .placements()
            .iter()
            .map(|p| (p.path.clone(), p.offset))
            .collect();
        let expected: Vec<(String, u64)> = [
            ("/EFI/BOOT/BOOTX64.EFI", 5u32),
            ("/EFI/BOOT/uefintfs.efi", 2228),
            ("/EFI/Rufus/exfat_x64.efi", 2292),
            ("/startup.nsh", 2374),
        ]
        .iter()
        .map(|(path, cluster)| {
            (path.to_string(), builder.cluster_offset(*cluster))
        })
        .collect();
        assert_eq!(placed, expected);
    }

    /// The whole 64 MiB volume, byte for byte, against p2 of the real image.
    ///
    /// This is the actual gate; everything above it is what still runs when the
    /// payload downloads are absent. Extract the partition first:
    ///
    /// ```text
    /// dd if=<image> bs=512 skip=12584960 count=131072 of=p2.bin
    /// OXWIN_TEST_ESP=p2.bin OXWIN_TEST_ASSETS=assets/efi cargo test
    /// ```
    #[test]
    fn matches_the_reference_partition_byte_for_byte() {
        let (Ok(esp), Ok(assets)) = (
            std::env::var("OXWIN_TEST_ESP"),
            std::env::var("OXWIN_TEST_ASSETS"),
        ) else {
            eprintln!("skipped: set OXWIN_TEST_ESP and OXWIN_TEST_ASSETS");
            return;
        };
        let reference = std::fs::read(&esp).unwrap();
        let assets = std::path::Path::new(&assets);
        let read = |name: &str| std::fs::read(assets.join(name)).unwrap();

        let mut b = Fat32Builder::new(Options {
            stamp: reference_stamp(),
            ..Options::esp(ESP_SECTORS)
        })
        .unwrap();
        b.add_file("/EFI/BOOT/BOOTX64.EFI", read("shellx64.efi")).unwrap();
        b.add_file("/EFI/BOOT/uefintfs.efi", read("uefintfs.efi")).unwrap();
        b.add_file("/EFI/Rufus/exfat_x64.efi", read("exfat_x64.efi")).unwrap();
        // The chooser script **as it was when this reference image was built** — not
        // as `builder::chooser_script()` writes it today, which has since gained lines
        // telling the operator what the blank screen means. This test proves the FAT32
        // writer reproduces a volume that booted, so its inputs are historical and must
        // stay that way. Do not "fix" this by pasting in the current script.
        let nsh = [
            "@echo -off",
            "echo OXIDE-CHOOSER: scanning for an installed Windows",
            "for %d in fs0 fs1 fs2 fs3 fs4 fs5 fs6 fs7",
            "  if exist %d:\\EFI\\Microsoft\\Boot\\bootmgfw.efi then",
            "    echo OXIDE-CHOOSER: BOOT-INSTALLED %d",
            "    echo OXIDE-CHOOSER: BOOT-INSTALLED %d",
            "    stall 3000000",
            "    %d:\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
            "  endif",
            "endfor",
            "echo OXIDE-CHOOSER: RUN-SETUP no installed Windows found",
            "echo OXIDE-CHOOSER: RUN-SETUP no installed Windows found",
            "stall 3000000",
            "EFI\\BOOT\\uefintfs.efi",
            "echo OXIDE-CHOOSER: FAILED Setup did not start; dropping to the shell",
            "stall 10000000",
        ]
        .join("\r\n")
            + "\r\n";
        b.add_file("/startup.nsh", nsh.into_bytes()).unwrap();

        let image = b.build().unwrap();
        let mut ours = Vec::new();
        image.write_dense(&mut ours).unwrap();
        if let Some(where_) = first_difference(&reference, &ours) {
            panic!("volume differs from p2 of the reference image: {where_}");
        }
    }

    #[test]
    fn a_volume_too_small_for_fat32_is_refused() {
        // The 1 MiB boot partition this project started with. Firmware would not read
        // it, and the reason was not obvious for some time.
        let err =
            Fat32Builder::new(Options::esp(2048)).err().unwrap().to_string();
        assert!(err.contains("65525"), "unhelpful error: {err}");
    }

    #[test]
    fn a_label_over_eleven_characters_is_refused() {
        let opts = Options {
            label: "TWELVECHARS!".to_string(),
            ..Options::esp(ESP_SECTORS)
        };
        assert!(Fat32Builder::new(opts).is_err());
    }

    #[test]
    fn contents_larger_than_the_volume_are_refused() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        b.reserve_file("/big.bin", 100 * 1024 * 1024).unwrap();
        let err = b.build().err().unwrap().to_string();
        assert!(err.contains("volume holds"), "unhelpful error: {err}");
    }

    #[test]
    fn a_file_over_four_gib_is_refused() {
        // The limit that sent this project to exFAT in the first place: install.wim is
        // 4.04 GiB and cannot exist on FAT32 at all.
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        assert!(b.reserve_file("/sources/install.wim", 4_340_202_461).is_err());
        assert!(
            b.reserve_file("/sources/install.wim", u32::MAX as u64).is_ok()
        );
    }

    #[test]
    fn short_names_match_the_reference_volume() {
        let taken = HashSet::new();
        let of = |name: &str, is_dir: bool| {
            String::from_utf8(short_name_for(name, is_dir, &taken).to_vec())
                .unwrap()
        };
        assert_eq!(of("BOOTX64.EFI", false), "BOOTX64 EFI");
        assert_eq!(of("uefintfs.efi", false), "UEFINTFSEFI");
        assert_eq!(of("startup.nsh", false), "STARTUP NSH");
        assert_eq!(of("Rufus", true), "RUFUS      ");
        // Nine characters of stem, so it truncates and takes the ~1 tail.
        assert_eq!(of("exfat_x64.efi", false), "EXFAT_~1EFI");
    }

    #[test]
    fn colliding_short_names_get_distinct_tails() {
        let mut taken = HashSet::new();
        let mut names = Vec::new();
        for name in
            ["longer name one.txt", "longer name two.txt", "longer name!.txt"]
        {
            let short = short_name_for(name, false, &taken);
            taken.insert(short);
            names.push(String::from_utf8(short.to_vec()).unwrap());
        }
        assert_eq!(names, ["LONGER~1TXT", "LONGER~2TXT", "LONGER~3TXT"]);
    }

    /// A directory's short name stops at its last dot, because that is what
    /// `fat32.js` does. Harmless for a mixed-case name — the long entry carries the
    /// real one — but an all-upper-case dotted directory needs no long entry and so
    /// is silently renamed. Nothing we put on the media has a dotted directory, and
    /// the two engines have to agree byte for byte while both exist, so this is
    /// pinned here rather than fixed on one side only.
    #[test]
    fn a_directory_short_name_stops_at_its_last_dot() {
        let taken = HashSet::new();
        assert_eq!(&short_name_for("Rufus.old", true, &taken), b"RUFUS      ");
        // Recoverable, because the mixed case forces a long entry.
        assert!(needs_lfn("Rufus.old"));
        // Not recoverable: no long entry, so `RUFUS.OLD` becomes `RUFUS`.
        assert_eq!(&short_name_for("RUFUS.OLD", true, &taken), b"RUFUS      ");
        assert!(!needs_lfn("RUFUS.OLD"));
    }

    #[test]
    fn checksums_match_the_reference_volume() {
        // The four long-name checksums read out of the reference volume's directories.
        assert_eq!(short_name_checksum(b"STARTUP NSH"), 217);
        assert_eq!(short_name_checksum(b"RUFUS      "), 154);
        assert_eq!(short_name_checksum(b"UEFINTFSEFI"), 243);
        assert_eq!(short_name_checksum(b"EXFAT_~1EFI"), 67);
    }

    #[test]
    fn only_names_that_need_a_long_entry_get_one() {
        for name in ["BOOTX64.EFI", "EFI", "BOOT", "A", "12345678.123"] {
            assert!(!needs_lfn(name), "{name} should not need a long name");
            assert_eq!(entries_for(name), 1);
        }
        for name in [
            "uefintfs.efi",    // lower case
            "Rufus",           // mixed case
            "TOOLONGSTEM.TXT", // nine-character stem
            "A.TOOLONG",       // four-character extension
            "TWO.DOTS.TXT",    // more than one dot
            "SPACE NAME.TXT",  // illegal character
            ".HIDDEN",         // empty stem
        ] {
            assert!(needs_lfn(name), "{name} should need a long name");
        }
        // Thirteen characters per entry, so fourteen takes two plus the short entry.
        assert_eq!(entries_for("uefintfs.efi"), 2);
        assert_eq!(entries_for("fourteen chars.txt"), 3);
    }

    #[test]
    fn a_long_name_is_split_across_entries_in_reverse() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        // Twenty characters: two entries, thirteen then seven plus a terminator.
        b.add_file("/abcdefghijklmnopqrst", Vec::new()).unwrap();
        b.assign_short_names(ROOT);
        let entries = b.lfn_entries(1);
        assert_eq!(entries.len(), 2);
        // The first entry on disk is the last of the sequence, flagged 0x40.
        assert_eq!(entries[0][0], 0x42);
        assert_eq!(entries[1][0], 0x01);
        assert_eq!(entries[0][11], ATTR_LONG_NAME);
        // The second chunk holds characters 14..=20, then the terminator, then padding.
        assert_eq!(entries[0][LFN_SLOTS[0]], b'n');
        assert_eq!(entries[0][LFN_SLOTS[6]], b't');
        assert_eq!(entries[0][LFN_SLOTS[7]], 0x00); // index 20 == length
        assert_eq!(entries[0][LFN_SLOTS[8]], 0xff);
        assert_eq!(entries[1][LFN_SLOTS[0]], b'a');
        // Both entries carry the short name's checksum, or readers discard the name.
        assert_eq!(entries[0][13], entries[1][13]);
    }

    #[test]
    fn an_empty_file_names_cluster_zero() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        b.add_file("/EMPTY.TXT", Vec::new()).unwrap();
        b.add_file("/AFTER.TXT", vec![1u8; 10]).unwrap();
        b.build().unwrap();
        let empty = b.child(ROOT, "EMPTY.TXT").unwrap();
        let after = b.child(ROOT, "AFTER.TXT").unwrap();
        assert_eq!(b.nodes[empty].first_cluster, 0);
        // And it consumes nothing, so the next file gets the cluster it would have had.
        assert_eq!(b.nodes[after].first_cluster, 3);
    }

    #[test]
    fn adding_the_same_path_twice_replaces_it_in_place() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        b.add_file("/sources/EI.CFG", b"first".to_vec()).unwrap();
        b.add_file("/A.TXT", b"a".to_vec()).unwrap();
        // Differing only in case, which a case-insensitive reader cannot tell apart.
        b.add_file("/sources/ei.cfg", b"second".to_vec()).unwrap();
        let sources = b.child(ROOT, "SOURCES").unwrap();
        assert_eq!(b.nodes[sources].children.len(), 1);
        let file = b.child(sources, "EI.CFG").unwrap();
        assert_eq!(b.nodes[file].data.as_deref(), Some(&b"second"[..]));
        // Position preserved: sources is still first, so cluster allocation and the
        // resulting image do not shift under a caller that overrides a file.
        assert_eq!(b.nodes[ROOT].children[0].0, "SOURCES");
    }

    #[test]
    fn a_file_where_a_directory_is_expected_is_refused() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        b.add_file("/EFI", b"not a directory".to_vec()).unwrap();
        assert!(b.add_file("/EFI/BOOT/X.EFI", Vec::new()).is_err());
    }

    #[test]
    fn dot_dot_names_the_root_as_cluster_zero() {
        let mut b = Fat32Builder::new(Options::esp(ESP_SECTORS)).unwrap();
        b.add_file("/EFI/BOOT/X.EFI", vec![7u8; 4]).unwrap();
        let image = b.build().unwrap();
        let efi = b.child(ROOT, "EFI").unwrap();
        let boot = b.child(efi, "BOOT").unwrap();

        // In /EFI, ".." is the root, which the spec spells as cluster 0 rather than 2.
        let at = b.cluster_offset(b.nodes[efi].first_cluster);
        let entry = image.read(at + ENTRY as u64, ENTRY);
        assert_eq!(&entry[0..11], b"..         ");
        assert_eq!(u16::from_le_bytes([entry[26], entry[27]]), 0);

        // In /EFI/BOOT it is the real cluster of /EFI.
        let at = b.cluster_offset(b.nodes[boot].first_cluster);
        let entry = image.read(at + ENTRY as u64, ENTRY);
        assert_eq!(&entry[0..11], b"..         ");
        assert_eq!(
            u16::from_le_bytes([entry[26], entry[27]]) as u32,
            b.nodes[efi].first_cluster
        );
    }

    /// The one thing a bare volume must get right: no partition table in front of the
    /// boot sector. `partition_start_lba` zero puts them at the same offset, and the
    /// boot sector has to be the one that survives.
    #[test]
    fn a_bare_volume_starts_with_its_boot_sector_not_a_partition_table() {
        let image = reference_esp().build().unwrap();
        let head = image.read(0, 11);
        assert_eq!(&head[0..3], &[0xeb, 0x58, 0x90]);
        assert_eq!(&head[3..11], b"MSWIN4.1");
        // 0x0c is the FAT32 partition type byte write_mbr would have left at 450.
        assert_ne!(image.read(450, 1)[0], 0x0c);
    }

    #[test]
    fn a_partitioned_volume_gets_a_table() {
        let mut b = Fat32Builder::new(Options {
            size_bytes: 128 * 1024 * 1024,
            partition_start_lba: 2048,
            sectors_per_cluster: 1,
            ..Options::default()
        })
        .unwrap();
        b.add_file("/A.TXT", b"a".to_vec()).unwrap();
        let image = b.build().unwrap();
        let mbr = image.read(0, 512);
        assert_eq!(mbr[446], 0x80, "bootable");
        assert_eq!(mbr[450], 0x0c, "FAT32 LBA");
        assert_eq!(u32::from_le_bytes(mbr[454..458].try_into().unwrap()), 2048);
        assert_eq!(
            u32::from_le_bytes(mbr[458..462].try_into().unwrap()),
            128 * 1024 * 1024 / 512 - 2048
        );
        assert_eq!(&mbr[510..], &[0x55, 0xaa]);
        // And the boot sector is at the partition, not at offset 0.
        assert_eq!(&image.read(2048 * SECTOR, 3), &[0xeb, 0x58, 0x90]);
    }

    #[test]
    fn timestamps_have_no_time_zone_in_them() {
        // The value fat32.js meant to write.
        assert_eq!(
            Timestamp::from_civil(2020, 1, 1, 0, 0, 0).unwrap(),
            Timestamp::MEDIA_EPOCH
        );
        // And the value it actually wrote, on a US/Eastern host.
        let stamp = reference_stamp();
        assert_eq!(stamp.time, 38912);
        assert_eq!(stamp.date, 20383);
        // Two-second resolution, rounding down.
        assert_eq!(
            Timestamp::from_civil(2020, 1, 1, 0, 0, 3).unwrap().time,
            Timestamp::from_civil(2020, 1, 1, 0, 0, 2).unwrap().time
        );
        assert!(Timestamp::from_civil(1979, 1, 1, 0, 0, 0).is_err());
        assert!(Timestamp::from_civil(2020, 13, 1, 0, 0, 0).is_err());
        assert!(Timestamp::from_civil(2020, 1, 1, 24, 0, 0).is_err());
    }

    #[test]
    fn paths_accept_either_separator_and_ignore_empty_parts() {
        assert_eq!(split_path("/EFI/BOOT/X.EFI"), ["EFI", "BOOT", "X.EFI"]);
        assert_eq!(split_path("EFI\\BOOT\\X.EFI"), ["EFI", "BOOT", "X.EFI"]);
        assert_eq!(split_path("//EFI//./BOOT//"), ["EFI", "BOOT"]);
        assert!(split_path("///").is_empty());
    }
}
