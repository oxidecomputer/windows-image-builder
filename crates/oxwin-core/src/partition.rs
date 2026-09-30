// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The target disk's partition layout.
//!
//! The answer file expresses this as three parallel hand-maintained lists:
//! `CreatePartition` carrying `Order`, and `ModifyPartition` carrying `Order` *and*
//! `PartitionID`. Exposing that shape for editing invites an off-by-one that Setup
//! does not catch, so **nothing here lets a user type an order or an ID**. The list
//! is ordered and both are derived from position, which deletes the error class
//! before any UI exists.
//!
//! What cannot be checked here: whether the partitions fit. The image is media; the
//! disk it installs onto is created on the rack afterwards and is never visible to
//! this code. "Your partitions do not fit" is a failure Setup discovers, and the UI
//! has to say so rather than imply a check it does not perform.

use crate::settings::Problem;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// The EFI system partition. Without one the firmware has nothing to boot.
    Efi,
    /// Microsoft Reserved. Carries no filesystem and gets no label or letter.
    Msr,
    Primary,
}

impl Kind {
    /// The `<Type>` value in `CreatePartition`, spelled as the schema wants it.
    pub fn type_name(self) -> &'static str {
        match self {
            Kind::Efi => "EFI",
            Kind::Msr => "MSR",
            Kind::Primary => "Primary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Fat32,
    Ntfs,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Fat32 => "FAT32",
            Format::Ntfs => "NTFS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub kind: Kind,
    /// Megabytes, or `None` for "take the rest of the disk" (`<Extend>true</Extend>`).
    pub size_mb: Option<u32>,
    pub label: Option<String>,
    pub letter: Option<char>,
    pub format: Option<Format>,
}

/// Exactly what the answer file has always written, and what every committed golden
/// carries. The EFI partition is 260 MB because that is the minimum FAT32 volume on
/// a 4Kn disk; 16 MB MSR and an extending NTFS system partition are the standard UEFI
/// layout.
///
/// Note what is *not* here: the WinRE recovery partition Windows normally creates.
/// That is existing behavior, not a decision taken by this module.
pub fn default_layout() -> Vec<Partition> {
    vec![
        Partition {
            kind: Kind::Efi,
            size_mb: Some(260),
            label: Some("System".into()),
            letter: None,
            format: Some(Format::Fat32),
        },
        Partition {
            kind: Kind::Msr,
            size_mb: Some(16),
            label: None,
            letter: None,
            format: None,
        },
        Partition {
            kind: Kind::Primary,
            size_mb: None,
            label: Some("Windows".into()),
            letter: Some('C'),
            format: Some(Format::Ntfs),
        },
    ]
}

/// The 1-based `PartitionID` Windows is installed to.
///
/// This was a hardcoded `3` in `<InstallTo>`, which is right for exactly one layout.
/// The OS partition is the one lettered `C`; failing that, the last `Primary`, so
/// the answer file still names something that exists.
pub fn os_partition_id(layout: &[Partition]) -> u32 {
    let lettered = layout
        .iter()
        .position(|p| p.kind == Kind::Primary && p.letter == Some('C'));
    let last_primary = layout.iter().rposition(|p| p.kind == Kind::Primary);
    (lettered.or(last_primary).unwrap_or(0) + 1) as u32
}

/// Three refusals and nine warnings.
///
/// The three refusals are the cases that produce a disk that cannot boot at all. On
/// a guest with no framebuffer the only symptom is a black screen, which looks like
/// every other failure, so letting those through costs a rack cycle to learn
/// nothing. Everything else warns: the user typed a layout on purpose.
pub fn problems(layout: &[Partition]) -> Vec<Problem> {
    let mut v = Vec::new();

    if !layout.iter().any(|p| p.kind == Kind::Efi) {
        v.push(Problem::block(
            "partitions",
            "No EFI system partition. An Oxide instance boots UEFI only, so the \
             firmware would have nothing to load and the guest shows nothing at \
             all.",
        ));
    }
    if !layout.iter().any(|p| p.kind == Kind::Primary) {
        v.push(Problem::block(
            "partitions",
            "No Primary partition, so there is nowhere to install Windows.",
        ));
    }

    // An ESP that is not FAT32 is a disk that cannot boot, for the same reason as
    // one that is absent: UEFI firmware reads FAT, nothing else, so it finds no
    // loader and the guest shows a black screen and no message at all. This is
    // reachable by a couple of clicks -- add a partition, set its kind to EFI --
    // and by `--partition=efi:260`, which leaves the format unset.
    if let Some(efi) = layout.iter().find(|p| p.kind == Kind::Efi) {
        if efi.format != Some(Format::Fat32) {
            v.push(Problem::block(
                "partitions",
                match efi.format {
                    Some(f) => format!(
                        "The EFI system partition is formatted {}. UEFI firmware \
                         reads FAT32 and nothing else, so it would find no loader \
                         and the guest would show nothing at all.",
                        f.name()
                    ),
                    None => "The EFI system partition has no format. UEFI \
                             firmware reads FAT32; unformatted, it would hold no \
                             loader and the guest would show nothing at all."
                        .to_string(),
                },
            ));
        }
        if efi.size_mb.is_some_and(|mb| mb < 100) {
            v.push(Problem::warn(
                "partition_efi_size",
                "The EFI partition is under 100 MB. 260 MB is the minimum FAT32 \
                 volume on a 4Kn disk and the size Windows itself uses.",
            ));
        }
    }
    if layout.first().is_some_and(|p| p.kind != Kind::Efi)
        && layout.iter().any(|p| p.kind == Kind::Efi)
    {
        v.push(Problem::warn(
            "partition_efi_order",
            "The EFI partition is not first. Firmware finds it either way, but \
             every tool that inspects this disk will expect it at the front.",
        ));
    }
    if !layout.iter().any(|p| p.kind == Kind::Msr) {
        v.push(Problem::warn(
            "partition_msr",
            "No Microsoft Reserved partition. Windows disk tooling expects one on \
             a GPT disk and some operations need it later.",
        ));
    }
    // An MSR carries no filesystem, so it can carry neither a label nor a letter
    // either. Setting kind to MSR in the editor leaves whatever the row had.
    if layout.iter().any(|p| {
        p.kind == Kind::Msr
            && (p.label.is_some() || p.letter.is_some() || p.format.is_some())
    }) {
        v.push(Problem::warn(
            "partition_msr_fields",
            "A Microsoft Reserved partition has a label, a drive letter or a \
             format. It holds no filesystem, so Setup has nothing to apply any \
             of those to.",
        ));
    }
    // Unformatted, Setup has nowhere to lay the image down.
    if layout.iter().any(|p| p.kind == Kind::Primary && p.format.is_none()) {
        v.push(Problem::warn(
            "partition_primary_format",
            "A Primary partition has no format. Windows installs onto NTFS, and \
             an unformatted partition is not somewhere Setup can put it.",
        ));
    }

    let extending: Vec<usize> = layout
        .iter()
        .enumerate()
        .filter(|(_, p)| p.size_mb.is_none())
        .map(|(i, _)| i)
        .collect();
    if extending.len() > 1 {
        v.push(Problem::warn(
            "partition_extend",
            "More than one partition is set to take the rest of the disk. Only \
             the first can, and what Setup does with the others is not defined.",
        ));
    }
    if extending.iter().any(|i| *i + 1 != layout.len()) {
        v.push(Problem::warn(
            "partition_extend_last",
            "A partition set to take the rest of the disk is not the last one, so \
             everything after it has no room left.",
        ));
    }

    let letters: Vec<char> = layout.iter().filter_map(|p| p.letter).collect();
    for (i, letter) in letters.iter().enumerate() {
        if letters[i + 1..].contains(letter) {
            v.push(Problem::warn(
                "partition_letter",
                format!("Two partitions are both lettered {letter}."),
            ));
            break;
        }
    }
    if !letters.contains(&'C') {
        v.push(Problem::warn(
            "partition_letter_c",
            "No partition is lettered C. Windows installs to C by convention and \
             the guest bootstrap writes C:\\oxide-bootstrap.log.",
        ));
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(problems: &[Problem]) -> Vec<&str> {
        problems.iter().map(|p| p.field).collect()
    }

    /// Exactly what the answer file has always written. Everything else in this
    /// phase is measured against these bytes.
    #[test]
    fn the_default_layout_is_efi_msr_primary() {
        let l = default_layout();
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].kind, Kind::Efi);
        assert_eq!(l[0].size_mb, Some(260));
        assert_eq!(l[0].label.as_deref(), Some("System"));
        assert_eq!(l[0].format, Some(Format::Fat32));
        assert_eq!(l[1].kind, Kind::Msr);
        assert_eq!(l[1].size_mb, Some(16));
        assert_eq!(l[1].label, None);
        assert_eq!(l[1].format, None);
        assert_eq!(l[2].kind, Kind::Primary);
        assert_eq!(l[2].size_mb, None, "the OS partition extends");
        assert_eq!(l[2].letter, Some('C'));
        assert_eq!(l[2].format, Some(Format::Ntfs));
    }

    #[test]
    fn the_default_layout_is_clean() {
        let found = problems(&default_layout());
        assert!(found.is_empty(), "false positives: {found:?}");
    }

    /// Blocking. Without an ESP the firmware has nothing to boot and the guest shows
    /// a black screen -- which, with no framebuffer, is indistinguishable from every
    /// other failure.
    #[test]
    fn no_efi_partition_blocks() {
        let l: Vec<Partition> = default_layout()
            .into_iter()
            .filter(|p| p.kind != Kind::Efi)
            .collect();
        let found = problems(&l);
        assert!(found.iter().any(|p| p.field == "partitions" && p.blocking));
    }

    /// Blocking for the same reason: there is nowhere to install Windows.
    #[test]
    fn no_primary_partition_blocks() {
        let l: Vec<Partition> = default_layout()
            .into_iter()
            .filter(|p| p.kind != Kind::Primary)
            .collect();
        assert!(problems(&l).iter().any(|p| p.blocking));
    }

    /// Blocking, and for the same reason as a missing one: UEFI firmware reads
    /// FAT32 and nothing else, so an NTFS ESP holds a loader nothing can find. The
    /// GUI could reach this in two clicks before the format picker existed.
    #[test]
    fn an_efi_that_is_not_fat32_blocks() {
        let mut l = default_layout();
        l[0].format = Some(Format::Ntfs);
        let found = problems(&l);
        assert!(found.iter().any(|p| p.field == "partitions" && p.blocking));
        assert!(found.iter().any(|p| p.message.contains("NTFS")));

        // Unformatted is the `--partition=efi:260` case, and just as unbootable.
        l[0].format = None;
        assert!(
            problems(&l).iter().any(|p| p.field == "partitions" && p.blocking)
        );
    }

    #[test]
    fn an_msr_with_a_label_letter_or_format_warns() {
        for mutate in [
            (|p: &mut Partition| p.label = Some("Reserved".into()))
                as fn(&mut Partition),
            |p: &mut Partition| p.letter = Some('R'),
            |p: &mut Partition| p.format = Some(Format::Ntfs),
        ] {
            let mut l = default_layout();
            mutate(&mut l[1]);
            let found = problems(&l);
            assert!(
                fields(&found).contains(&"partition_msr_fields"),
                "no finding for {found:?}"
            );
            assert!(found.iter().all(|p| !p.blocking));
        }
    }

    #[test]
    fn a_primary_with_no_format_warns() {
        let mut l = default_layout();
        l[2].format = None;
        let found = problems(&l);
        assert!(fields(&found).contains(&"partition_primary_format"));
        assert!(found.iter().all(|p| !p.blocking));
    }

    #[test]
    fn a_tiny_efi_warns() {
        let mut l = default_layout();
        l[0].size_mb = Some(50);
        let found = problems(&l);
        assert!(fields(&found).contains(&"partition_efi_size"));
        assert!(found.iter().all(|p| !p.blocking));
    }

    #[test]
    fn efi_not_first_warns() {
        let mut l = default_layout();
        l.swap(0, 1);
        assert!(fields(&problems(&l)).contains(&"partition_efi_order"));
    }

    #[test]
    fn a_missing_msr_warns() {
        let l: Vec<Partition> = default_layout()
            .into_iter()
            .filter(|p| p.kind != Kind::Msr)
            .collect();
        assert!(fields(&problems(&l)).contains(&"partition_msr"));
    }

    #[test]
    fn two_extending_partitions_warn() {
        let mut l = default_layout();
        l.push(Partition {
            kind: Kind::Primary,
            size_mb: None,
            label: Some("Data".into()),
            letter: Some('D'),
            format: Some(Format::Ntfs),
        });
        assert!(fields(&problems(&l)).contains(&"partition_extend"));
    }

    /// An extending partition that is not last leaves everything after it with no
    /// room, and Setup fails at a point that names none of this.
    #[test]
    fn an_extend_that_is_not_last_warns() {
        let mut l = default_layout();
        l.push(Partition {
            kind: Kind::Primary,
            size_mb: Some(1024),
            label: Some("Data".into()),
            letter: Some('D'),
            format: Some(Format::Ntfs),
        });
        assert!(fields(&problems(&l)).contains(&"partition_extend_last"));
    }

    #[test]
    fn duplicate_letters_warn() {
        let mut l = default_layout();
        l.push(Partition {
            kind: Kind::Primary,
            size_mb: Some(1024),
            label: Some("Data".into()),
            letter: Some('C'),
            format: Some(Format::Ntfs),
        });
        assert!(fields(&problems(&l)).contains(&"partition_letter"));
    }

    #[test]
    fn no_c_drive_warns() {
        let mut l = default_layout();
        l[2].letter = Some('D');
        assert!(fields(&problems(&l)).contains(&"partition_letter_c"));
    }

    /// The bug this function exists to prevent. <InstallTo><PartitionID> was a
    /// hardcoded 3, correct only for the default layout.
    #[test]
    fn the_os_partition_id_is_one_based_and_follows_the_layout() {
        assert_eq!(os_partition_id(&default_layout()), 3);

        // No MSR: the OS partition is now the second one.
        let l: Vec<Partition> = default_layout()
            .into_iter()
            .filter(|p| p.kind != Kind::Msr)
            .collect();
        assert_eq!(os_partition_id(&l), 2);

        // Two primaries: C wins, whichever position it is in.
        let mut l = default_layout();
        l[2].letter = Some('D');
        l.push(Partition {
            kind: Kind::Primary,
            size_mb: None,
            label: Some("Windows".into()),
            letter: Some('C'),
            format: Some(Format::Ntfs),
        });
        assert_eq!(os_partition_id(&l), 4);
    }

    /// No C anywhere: fall back to the last Primary rather than to a literal, so
    /// the answer file still points somewhere that exists.
    #[test]
    fn the_os_partition_id_falls_back_to_the_last_primary() {
        let mut l = default_layout();
        l[2].letter = None;
        assert_eq!(os_partition_id(&l), 3);
    }

    /// C, not "the last Primary", wins when they are different partitions. Mutation
    /// testing found that `the_os_partition_id_is_one_based_and_follows_the_layout`
    /// does not catch dropping the `lettered.or(..)` preference, because in that
    /// test's final case C also happens to be the last Primary. This one separates
    /// the two: D is last, C is not, and C must still win.
    #[test]
    fn the_os_partition_id_prefers_c_over_a_later_unlettered_primary() {
        let mut l = default_layout();
        l.push(Partition {
            kind: Kind::Primary,
            size_mb: Some(1024),
            label: Some("Data".into()),
            letter: Some('D'),
            format: Some(Format::Ntfs),
        });
        assert_eq!(os_partition_id(&l), 3);
    }
}
