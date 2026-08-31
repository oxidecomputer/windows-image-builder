// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! What survives a successful run.
//!
//! Levels are cumulative and there is deliberately no `none`: the image is the
//! product of an hour of wall clock, and a level that deletes it has no use.
//!
//! This runs **only after the image exists**, never on a failure path. A transient
//! error forty minutes in must not discard the work, and a rollback that itself
//! fails leaves a state harder to reason about than a named resource.

use crate::golden::Names;
use anyhow::{Result, bail};
use std::str::FromStr;

/// One thing to remove, and what kind of thing it is.
///
/// Typed rather than a bare string because the *order* of deletion is the part
/// that matters, and an order over untyped names cannot be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    Instance(String),
    Disk(String),
    Snapshot(String),
    Image(String),
}

impl Resource {
    /// The `oxide` command that removes it, for the failure message.
    pub fn delete_command(&self, project: &str) -> String {
        match self {
            Resource::Instance(n) => format!(
                "oxide instance delete --project {project} --instance {n}"
            ),
            Resource::Disk(n) => {
                format!("oxide disk delete --project {project} --disk {n}")
            }
            Resource::Snapshot(n) => format!(
                "oxide snapshot delete --project {project} --snapshot {n}"
            ),
            Resource::Image(n) => {
                format!("oxide image delete --project {project} --image {n}")
            }
        }
    }
}

/// How much of a successful run to leave behind. Cumulative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keep {
    /// The image only. Everything temporary goes.
    #[default]
    Image,
    /// The image and the snapshot it came from.
    Snapshot,
    /// The above, plus the installer and system disks.
    Disks,
    /// Nothing is deleted.
    All,
}

impl Keep {
    /// What to delete, in the order that works.
    ///
    /// Instances first: a disk attached to an instance cannot be deleted.
    /// Snapshots after disks: a snapshot outlives the disk it came from, and the
    /// image may pin it. This is the ordering `Leftovers::cleanup_commands`
    /// already encodes.
    pub fn to_delete(&self, names: &Names) -> Vec<Resource> {
        let mut out = Vec::new();
        if *self != Keep::All {
            out.push(Resource::Instance(names.instance()));
        }
        if matches!(self, Keep::Image | Keep::Snapshot) {
            out.push(Resource::Disk(names.installer_disk()));
            out.push(Resource::Disk(names.system_disk()));
        }
        if *self == Keep::Image {
            out.push(Resource::Snapshot(names.snapshot()));
        }
        out
    }
}

impl FromStr for Keep {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "image" => Ok(Keep::Image),
            "snapshot" => Ok(Keep::Snapshot),
            "disks" => Ok(Keep::Disks),
            "all" => Ok(Keep::All),
            other => bail!(
                "--keep={other}: expected image, snapshot, disks or all. There \
                 is no `none`, because the image is what the run is for"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::Names;

    fn names() -> Names {
        Names::new("g").unwrap()
    }

    /// The default deletes everything except the product.
    #[test]
    fn keeping_the_image_deletes_the_rest() {
        let to_delete = Keep::Image.to_delete(&names());
        assert_eq!(
            to_delete,
            vec![
                Resource::Instance("g".into()),
                Resource::Disk("g-installer".into()),
                Resource::Disk("g-system".into()),
                Resource::Snapshot("g-snap".into()),
            ]
        );
    }

    /// Instances before disks, because a disk attached to an instance cannot be
    /// deleted; snapshots after disks, because a snapshot outlives its disk and an
    /// image may pin it. Getting this order wrong hands the user a sequence that
    /// fails on its first line.
    #[test]
    fn instances_come_before_disks_and_snapshots_come_last() {
        let to_delete = Keep::Image.to_delete(&names());
        let kind = |r: &Resource| match r {
            Resource::Instance(_) => 0,
            Resource::Disk(_) => 1,
            Resource::Snapshot(_) => 2,
            Resource::Image(_) => 3,
        };
        let order: Vec<u8> = to_delete.iter().map(kind).collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "out of order: {to_delete:?}");
    }

    #[test]
    fn the_levels_are_cumulative() {
        let n = names();
        assert!(
            Keep::Snapshot
                .to_delete(&n)
                .iter()
                .all(|r| !matches!(r, Resource::Snapshot(_)))
        );
        assert_eq!(
            Keep::Disks.to_delete(&n),
            vec![Resource::Instance("g".into())],
            "keeping the disks still removes the temporary instance"
        );
        assert!(Keep::All.to_delete(&n).is_empty());
    }

    /// The image is the product of an hour-long run. No level deletes it.
    #[test]
    fn no_level_ever_deletes_the_image() {
        let n = names();
        for keep in [Keep::Image, Keep::Snapshot, Keep::Disks, Keep::All] {
            assert!(
                !keep
                    .to_delete(&n)
                    .iter()
                    .any(|r| matches!(r, Resource::Image(_))),
                "{keep:?} would delete the image"
            );
        }
    }

    #[test]
    fn parsing_the_flag() {
        assert_eq!("image".parse::<Keep>().unwrap(), Keep::Image);
        assert_eq!("all".parse::<Keep>().unwrap(), Keep::All);
        // `none` was deliberately dropped: the image is the product, so a level
        // that removes it has no use. It must not silently mean something else.
        let error = "none".parse::<Keep>().unwrap_err().to_string();
        assert!(error.contains("image"), "{error}");
    }
}
