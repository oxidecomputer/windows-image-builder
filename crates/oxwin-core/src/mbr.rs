// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The MBR partition table.
//!
//! MBR rather than GPT, and only two entries: an exFAT media volume and a small FAT32
//! EFI System Partition. The ESP is the bootable one, which looks backwards until you
//! remember that UEFI firmware can only read FAT, it cannot see the exFAT volume at
//! all, so the thing it boots is the little partition carrying a filesystem driver for
//! the big one. Windows is also picky about which drives it loads from and their order.
//!
//! Verified against the first 512 bytes of an image that installed Windows on real
//! Oxide hardware; see the test at the bottom.

pub const SECTOR: usize = 512;

/// Where the partition entries begin. Everything before it is boot code, which we
/// leave zeroed: the firmware boots via the EFI System Partition, not via MBR boot
/// code, so there is nothing to put there.
const TABLE_OFFSET: usize = 446;
const ENTRY_SIZE: usize = 16;
const MAX_ENTRIES: usize = 4;

/// Partition type bytes we use.
pub mod kind {
    /// IFS — what exFAT volumes are tagged as. Also used for NTFS.
    pub const EXFAT: u8 = 0x07;
    /// EFI System Partition.
    pub const ESP: u8 = 0xef;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    pub bootable: bool,
    pub kind: u8,
    /// First sector, absolute LBA.
    pub start_lba: u32,
    pub sectors: u32,
}

/// Build the 512-byte boot sector for `partitions`.
///
/// The CHS fields are filled with the 0xFE/0xFF/0xFF "beyond CHS addressing" sentinel
/// rather than computed geometry. Every partition here starts well past the 8 GB CHS
/// limit's usefulness and all modern firmware reads the LBA fields, so real CHS values
/// would be fiction that some tool might believe.
pub fn boot_sector(partitions: &[Partition]) -> [u8; SECTOR] {
    assert!(
        partitions.len() <= MAX_ENTRIES,
        "MBR holds {MAX_ENTRIES} partitions, got {}",
        partitions.len()
    );
    let mut mbr = [0u8; SECTOR];
    for (slot, p) in partitions.iter().enumerate() {
        let at = TABLE_OFFSET + slot * ENTRY_SIZE;
        mbr[at] = if p.bootable { 0x80 } else { 0x00 };
        mbr[at + 1..at + 4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        mbr[at + 4] = p.kind;
        mbr[at + 5..at + 8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        mbr[at + 8..at + 12].copy_from_slice(&p.start_lba.to_le_bytes());
        mbr[at + 12..at + 16].copy_from_slice(&p.sectors.to_le_bytes());
    }
    // The boot signature. Without it firmware treats the disk as unpartitioned.
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    mbr
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes 446..512 of `ws2022-win-server-01-install.img`, an image that booted,
    /// installed Windows Server 2022 unattended, and came up with networking and RDP
    /// on an Oxide rack. Embedded rather than read from disk so this test is hermetic
    /// and always runs.
    const REFERENCE_TAIL: &[u8] = &[
        0x00, 0xfe, 0xff, 0xff, 0x07, 0xfe, 0xff, 0xff, 0x00, 0x08, 0x00, 0x00,
        0x00, 0x00, 0xc0, 0x00, 0x80, 0xfe, 0xff, 0xff, 0xef, 0xfe, 0xff, 0xff,
        0x00, 0x08, 0xc0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x55, 0xaa,
    ];

    /// The layout that image was built with.
    fn reference_partitions() -> Vec<Partition> {
        vec![
            Partition {
                bootable: false,
                kind: kind::EXFAT,
                start_lba: 2048,
                sectors: 12_582_912,
            },
            Partition {
                bootable: true,
                kind: kind::ESP,
                start_lba: 12_584_960,
                sectors: 131_072,
            },
        ]
    }

    #[test]
    fn matches_an_image_that_booted_on_real_hardware() {
        let mbr = boot_sector(&reference_partitions());
        assert_eq!(
            &mbr[446..],
            REFERENCE_TAIL,
            "partition table differs from the reference image"
        );
        // The reference image's boot code area is entirely zero, and so is ours.
        assert!(mbr[..446].iter().all(|b| *b == 0));
    }

    #[test]
    fn the_esp_is_the_bootable_partition() {
        // Backwards-looking but essential: firmware cannot read exFAT, so booting p1
        // is impossible. If this ever flips, nothing boots at all.
        let mbr = boot_sector(&reference_partitions());
        assert_eq!(mbr[446], 0x00, "p1 must not be bootable");
        assert_eq!(mbr[446 + 16], 0x80, "p2 must be bootable");
        assert_eq!(mbr[446 + 16 + 4], kind::ESP);
    }

    #[test]
    fn signature_is_present_even_with_no_partitions() {
        let mbr = boot_sector(&[]);
        assert_eq!(&mbr[510..], &[0x55, 0xaa]);
        assert!(mbr[..510].iter().all(|b| *b == 0));
    }

    #[test]
    fn little_endian_lba_and_length() {
        let mbr = boot_sector(&[Partition {
            bootable: false,
            kind: kind::EXFAT,
            start_lba: 0x0201_0000,
            sectors: 0x0403_0000,
        }]);
        assert_eq!(&mbr[446 + 8..446 + 12], &[0x00, 0x00, 0x01, 0x02]);
        assert_eq!(&mbr[446 + 12..446 + 16], &[0x00, 0x00, 0x03, 0x04]);
    }

    #[test]
    #[should_panic(expected = "MBR holds 4 partitions")]
    fn refuses_a_fifth_partition() {
        let p = Partition {
            bootable: false,
            kind: kind::EXFAT,
            start_lba: 1,
            sectors: 1,
        };
        boot_sector(&[p, p, p, p, p]);
    }
}
