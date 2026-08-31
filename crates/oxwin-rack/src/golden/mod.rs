// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The golden-image cycle: media in, a reusable Windows image out.
//!
//! Seven steps — build, upload, instance, watch, snapshot, image, teardown — each
//! idempotent, each reconciled against the rack before it runs. See
//! `docs/superpowers/specs/2026-08-31-golden-image-automation-design.md`.

pub mod keep;
pub mod names;
pub mod reconcile;
pub mod steps;
pub mod watch;

pub use keep::{Keep, Resource};
pub use names::Names;
pub use reconcile::{DiskStatus, Existing, Step, next_step};
pub use steps::{pick_address, port_open};
pub use watch::{Action, Milestone, Watch};

use crate::upload::Rack;
use anyhow::Result;
use oxwin_core::progress::Reporter;

impl Rack {
    /// Snapshot the system disk of a stopped, generalized instance.
    ///
    /// Idempotent: an existing snapshot of this name is used as-is. That is what
    /// makes a resume after a failed image creation cost nothing rather than
    /// collide with a name that is already taken.
    pub fn snapshot_step(
        &self,
        names: &Names,
        reporter: &Reporter,
    ) -> Result<uuid::Uuid> {
        if let Some(id) = self.snapshot_id(&names.snapshot())? {
            reporter.phase(
                "snapshot",
                format!("{} already exists; using it", names.snapshot()),
            );
            return Ok(id);
        }
        reporter.phase(
            "snapshot",
            format!(
                "snapshotting {} as {}",
                names.system_disk(),
                names.snapshot()
            ),
        );
        self.create_snapshot(&names.system_disk(), &names.snapshot())
    }

    /// Make the image. The product of the whole cycle.
    ///
    /// `os` and `version` are what the image reports about itself later. The
    /// release is detected from the media when the run started from an ISO; from a
    /// prebuilt `.img` there is nothing to detect, so the caller passes what it
    /// knows.
    pub fn image_step(
        &self,
        names: &Names,
        os: &str,
        version: &str,
        reporter: &Reporter,
    ) -> Result<()> {
        if self.image_exists(&names.image())? {
            reporter.phase(
                "image",
                format!("{} already exists; nothing to do", names.image()),
            );
            return Ok(());
        }
        let id = self.snapshot_id(&names.snapshot())?.ok_or_else(|| {
            anyhow::anyhow!(
                "there is no snapshot called {} to make an image from",
                names.snapshot()
            )
        })?;
        reporter.phase(
            "image",
            format!(
                "creating image {} from {}",
                names.image(),
                names.snapshot()
            ),
        );
        self.create_image(&names.image(), id, os, version)
    }

    /// Remove what `keep` says need not survive.
    ///
    /// **Only ever called after the image exists.** Never on a failure path: a
    /// transient error forty minutes into an hour-long run must not discard the
    /// work, and a rollback that itself fails leaves a state harder to reason about
    /// than a named resource left in place.
    ///
    /// Returns what it could not remove rather than failing. By this point the run
    /// has succeeded — the image is made — and reporting a leftover disk as a
    /// failed run would send someone looking for a problem with their image.
    pub fn teardown_step(
        &self,
        names: &Names,
        keep: Keep,
        reporter: &Reporter,
    ) -> Result<Vec<Resource>> {
        let mut stuck = Vec::new();
        for resource in keep.to_delete(names) {
            reporter.phase("teardown", format!("deleting {resource:?}"));
            if let Err(e) = self.delete(&resource) {
                reporter.log(format!("could not delete {resource:?}: {e}"));
                stuck.push(resource);
            }
        }
        Ok(stuck)
    }
}

#[cfg(test)]
mod tests {
    /// Both step functions must consult the rack before creating, or a resume makes
    /// a second snapshot with a name that already exists and fails on a run that had
    /// already succeeded.
    ///
    /// There is no rack in `cargo test`, so this pins the shape rather than the
    /// behaviour: each step's source contains its existence check. A source-text
    /// test is weak evidence and is here only because the strong evidence needs an
    /// hour of rack time — the real check is running the same command twice, which
    /// is what the rack procedure in DEVELOPMENT.md asks for.
    #[test]
    fn the_steps_check_before_they_create() {
        let source = include_str!("mod.rs");
        for (function, check) in [
            ("fn snapshot_step", "snapshot_id"),
            ("fn image_step", "image_exists"),
        ] {
            let body = source
                .split(function)
                .nth(1)
                .unwrap_or_else(|| panic!("{function} is missing"));
            assert!(
                body[..body.len().min(1200)].contains(check),
                "{function} must call {check} before creating anything"
            );
        }
    }
}
