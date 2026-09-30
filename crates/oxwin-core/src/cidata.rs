// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! A NoCloud config drive, for testing cloud-init without a rack.
//!
//! An Oxide instance gets its config drive from the control plane, so nothing
//! here ships in an image: this exists so `tools/qemu-test.sh --cloud-init`
//! can attach a real `cidata` volume and exercise the MSI install, the service
//! ordering, the profile, the hostname, the keys and `user_data` against
//! actual cloudbase-init. A rack test is still the gate; this is what makes a
//! rack cycle worth spending.
//!
//! **The label.** cloudbase-init's `is_label` in
//! `cloudbaseinit/utils/windows/vfat.py:52-53` (tag `1.1.8`) builds
//! `[drive_label.lower(), drive_label.upper()]` and accepts either: exactly
//! `cidata` or exactly `CIDATA`, never a mixed case. `fat32::Options` upper-cases
//! whatever label it is given, so `LABEL` below is written as `CIDATA` and no
//! change to `fat32.rs` is needed.
//!
//! **`meta-data`'s shape.** `NoCloudConfigDriveService.get_public_keys` in
//! `cloudbaseinit/metadata/services/nocloudservice.py:690-698` (same tag) reads
//! `meta-data`'s `public-keys` key and, when it is already a list, returns it
//! directly (`raw_ssh_keys` -- the `isinstance(raw_ssh_keys, list)` branch at
//! line 695-696). Only when it is a mapping does it go looking for an
//! `openssh-key` field per entry. `meta_data` below therefore emits a plain
//! YAML list, the simpler shape upstream already prefers.
//! `get_host_name`/`get_instance_id` (lines 684-688) read `local-hostname` and
//! `instance-id` the same way.

use crate::fat32::{Fat32Builder, Options, Timestamp};
use anyhow::Result;

/// The one label cloudbase-init's NoCloud vfat lookup accepts, upper-cased. See
/// the module doc for where that was verified.
pub const LABEL: &str = "CIDATA";

/// ~64 MiB with 512-byte clusters, matching `Options::esp`: FAT32 needs at
/// least 65 525 clusters, so a smaller volume is not a FAT32 volume.
///
/// `fat32.rs` writes a fixed CHS geometry into the boot sector -- 63 sectors
/// per track, 255 heads, shared with the ESP whose golden must not move --
/// and mtools' own sanity check (`vfat.c`'s "sectors per track" test)
/// refuses to read *any* FAT volume whose total sector count is not an
/// exact multiple of that 63. A plain 64 MiB (131072 sectors) fails it:
/// 131072 / 63 is not an integer, and cloudbase-init calls `mlabel` directly
/// with no way to pass `mtools_skip_check=1`. `Fat32Builder::new` places no
/// power-of-two requirement on `size_bytes` (`geometry_for` in `fat32.rs`
/// only ever divides and rounds), so the fix is to round the sector count
/// to a multiple of 63 rather than to pick a different cluster size.
///
/// `SparseImage::new` (`sparse.rs`) separately requires the image size be a
/// multiple of its 512 KiB (1024-sector) block, so the count also has to be
/// a multiple of 1024. `lcm(63, 1024) == 64512` sectors (they share no
/// factor), and two such tracks-of-blocks -- 129024 sectors, 63 MiB -- is
/// the closest multiple of both to 64 MiB and comfortably above the
/// 65 525-cluster FAT32 floor.
const SECTORS: u32 = 129024;

/// What a local NoCloud drive needs. The rack's own config drive comes from
/// the control plane and never goes through this struct.
pub struct Drive {
    pub hostname: String,
    pub instance_id: String,
    pub keys: Vec<String>,
    pub user_data: Option<String>,
}

/// A YAML double-quoted scalar. `instance-id`, `local-hostname` and each key
/// are free text -- a hostname or a comment on a key can legally contain
/// `": "`, a leading `"- "`, a leading `*`, or a `" #"`, every one of which
/// breaks a plain (unquoted) YAML scalar: the first three refuse to parse at
/// all, and PyYAML silently truncates the value at a whitespace-preceded
/// `#`. Double-quoting escapes only `\` and `"`, which is everything a
/// control-character-free value can contain that YAML's double-quoted form
/// treats specially. The pattern already exists once in this crate, for a
/// PowerShell single-quoted string: `cloudinit.rs`'s `ps_quote`.
fn yaml_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The `meta-data` file: `instance-id`, then `local-hostname`, then
/// `public-keys` as a YAML list, in that fixed order. LF line endings --
/// cloudbase-init runs on Windows but reads this with a YAML parser, which
/// does not care about line endings, and a fixed order is what makes
/// [`the_same_drive_builds_the_same_bytes`] a meaningful test rather than a
/// coincidence. Every free-text value is a double-quoted YAML scalar -- see
/// [`yaml_quote`] -- because a hostname or a key comment is not writer-
/// controlled text and any of it can otherwise break the parse or be
/// silently truncated.
pub fn meta_data(drive: &Drive) -> String {
    let mut lines = vec![
        format!("instance-id: {}", yaml_quote(&drive.instance_id)),
        format!("local-hostname: {}", yaml_quote(&drive.hostname)),
    ];
    if !drive.keys.is_empty() {
        lines.push("public-keys:".to_string());
        for key in &drive.keys {
            lines.push(format!("  - {}", yaml_quote(key)));
        }
    }
    lines.join("\n") + "\n"
}

/// Build a complete raw disk image of a bare vFAT `CIDATA` volume: no
/// partition table, `meta-data` then `user-data` (empty when `user_data` is
/// `None` -- NoCloud wants the file present even with nothing in it).
pub fn build(drive: &Drive) -> Result<Vec<u8>> {
    let mut builder = Fat32Builder::new(Options {
        label: LABEL.to_string(),
        stamp: Timestamp::MEDIA_EPOCH,
        ..Options::esp(SECTORS)
    })?;
    builder.add_file("/meta-data", meta_data(drive).into_bytes())?;
    builder.add_file(
        "/user-data",
        drive.user_data.clone().unwrap_or_default().into_bytes(),
    )?;
    let image = builder.build()?;
    let mut bytes = Vec::new();
    image.write_dense(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BPB's hardcoded sectors-per-track (`fat32.rs`, boot-sector offset
    /// 24) and the sector size, needed here to check the arithmetic that
    /// [`super::SECTORS`] was chosen to satisfy -- see its doc comment.
    const SECTORS_PER_TRACK: u32 = 63;
    const SECTOR: u64 = 512;

    fn drive() -> Drive {
        Drive {
            hostname: "clone-01".into(),
            instance_id: "i-0xide0001".into(),
            keys: vec!["ssh-ed25519 AAAAC3Nz dan@example".into()],
            user_data: Some("#cloud-config\nfinal_message: hello\n".into()),
        }
    }

    /// NoCloud reads `local-hostname` and `public-keys` out of `meta-data`.
    /// `instance-id` is what makes a re-run a no-op rather than a repeat, so it
    /// is not optional.
    #[test]
    fn the_metadata_carries_the_hostname_the_id_and_the_keys() {
        let meta = meta_data(&drive());
        assert!(meta.contains(r#"instance-id: "i-0xide0001""#));
        assert!(meta.contains(r#"local-hostname: "clone-01""#));
        assert!(meta.contains(r#""ssh-ed25519 AAAAC3Nz dan@example""#));
    }

    /// `instance-id`, `local-hostname` and each key are free text, not
    /// writer-controlled, and every one of `": "`, a leading `"- "`, a
    /// leading `*` and a whitespace-preceded `#` either breaks a plain YAML
    /// scalar outright or -- for `#` -- truncates it silently. Quoting is
    /// what keeps a hostname or a key comment from defeating the one thing
    /// this generator exists to give: a clean signal before a rack cycle.
    #[test]
    fn hostile_values_are_yaml_quoted_not_mangled() {
        let hostile = Drive {
            hostname: r#"evil: host # comment"#.into(),
            instance_id: "- leading dash and a \"quote\" and a \\ backslash"
                .into(),
            keys: vec!["*alias: not really # a key".into()],
            user_data: None,
        };
        let meta = meta_data(&hostile);
        let lines: Vec<&str> = meta.lines().collect();
        assert_eq!(
            lines[0],
            r#"instance-id: "- leading dash and a \"quote\" and a \\ backslash""#
        );
        assert_eq!(lines[1], r#"local-hostname: "evil: host # comment""#);
        assert_eq!(lines[2], "public-keys:");
        assert_eq!(lines[3], r#"  - "*alias: not really # a key""#);
    }

    /// The label is what cloudbase-init looks for. Get it wrong and the drive
    /// is never found, silently -- which is the whole failure mode this
    /// generator exists to rule out before a rack cycle is spent on it.
    ///
    /// `0x47` is `BS_VolLab` in the FAT32 boot sector: `fat32.rs`'s
    /// `write_boot_sectors` writes the label at `boot[71..82]` (`71 == 0x47`),
    /// at `base = partition_start_lba * SECTOR`, and `Options::esp` sets
    /// `partition_start_lba` to zero, so the boot sector -- and this field --
    /// sit at absolute offset 0 in a bare volume like this one.
    #[test]
    fn the_volume_is_labelled_for_no_cloud() {
        let image = build(&drive()).unwrap();
        let label: Vec<u8> = image[0x47..0x47 + 11].to_vec();
        assert_eq!(
            String::from_utf8_lossy(&label).trim_end(),
            LABEL,
            "the FAT32 boot sector's volume label"
        );
    }

    /// The root directory also carries a volume-label entry (FAT32's
    /// `ATTR_VOLUME_ID` entry, written by `volume_label_entry` at cluster 2 --
    /// `data_start_sector` for this geometry, sector 2032) and that is the
    /// copy Windows itself reports, not the boot sector's.
    #[test]
    fn the_root_directory_carries_the_same_label() {
        let image = build(&drive()).unwrap();
        // data_start_sector for this 129024-sector, 1-sector-cluster geometry
        // is 2032: reserved_sectors (32) plus two 1000-sector FATs. Recompute
        // rather than trust this comment if SECTORS or reserved_sectors ever
        // change -- geometry_for's arithmetic in fat32.rs is the source of
        // truth.
        let at = (2032 * SECTOR) as usize;
        let entry = &image[at..at + 11];
        assert_eq!(String::from_utf8_lossy(entry).trim_end(), LABEL);
    }

    /// mtools' own sanity check -- the exact failure this task fixes --
    /// refuses a FAT volume whose BPB-declared total sector count
    /// (`BPB_TotSec32`, boot-sector offset 32) is not a multiple of its
    /// declared sectors per track (offset 24). `fat32.rs` hardcodes 63
    /// there; regenerate the volume with `SECTORS` accordingly whenever this
    /// starts failing rather than special-casing the check away.
    #[test]
    fn total_sectors_is_a_whole_number_of_tracks() {
        let image = build(&drive()).unwrap();
        let sectors_per_track =
            u32::from(u16::from_le_bytes([image[24], image[25]]));
        let total_sectors =
            u32::from_le_bytes([image[32], image[33], image[34], image[35]]);
        assert_eq!(sectors_per_track, SECTORS_PER_TRACK);
        assert_eq!(total_sectors, SECTORS);
        assert_eq!(
            total_sectors % sectors_per_track,
            0,
            "mtools' mlabel refuses this exact mismatch"
        );
    }

    #[test]
    fn a_drive_with_no_user_data_still_builds() {
        let image = build(&Drive { user_data: None, ..drive() }).unwrap();
        assert!(!image.is_empty());
    }

    /// Two builds of the same drive are the same bytes, like every other
    /// volume this workspace writes.
    #[test]
    fn the_same_drive_builds_the_same_bytes() {
        assert_eq!(build(&drive()).unwrap(), build(&drive()).unwrap());
    }
}
