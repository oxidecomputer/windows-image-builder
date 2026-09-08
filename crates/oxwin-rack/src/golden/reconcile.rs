// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! What exists on the rack, and therefore what happens next.
//!
//! This is resumability. There is no journal recording where a run got to, so
//! we check disks on the rack. This process doesnt take a ton of time, so
//! resuming is a best effort here.
//!
//! Ordered latest-first: the furthest-along evidence wins, so a run resumed after
//! the image was made does the teardown rather than starting over.

use anyhow::{Result, bail};
use oxide::types::{DiskState, InstanceState};

/// A disk, as far as this cycle is concerned.
///
/// The SDK's `DiskState` carries an attachment id and does not implement
/// `PartialEq`, and neither fact matters here: what the cycle needs to know is
/// whether a disk is finished enough to use. Classifying at the boundary keeps the
/// deciding code comparable and testable, and puts the judgement about which
/// states are usable in one place with a reason next to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskStatus {
    /// Detached or attached — a finished disk.
    Usable,
    /// Mid-import or mid-transition. Carries the state's name so the refusal can
    /// quote it back.
    Unusable(String),
}

impl DiskStatus {
    pub fn of(state: &DiskState) -> Self {
        match state {
            DiskState::Detached | DiskState::Attached(_) => DiskStatus::Usable,
            other => DiskStatus::Unusable(format!("{other:?}")),
        }
    }
}

/// What the rack currently has, for one run's names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Existing {
    pub installer: Option<DiskStatus>,
    pub system: Option<DiskStatus>,
    pub instance: Option<InstanceState>,
    pub snapshot: bool,
    pub image: bool,
}

/// The next thing to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Upload,
    CreateInstance,
    Watch,
    Snapshot,
    CreateImage,
    Teardown,
}

pub fn next_step(existing: &Existing) -> Result<Step> {
    // Latest first.
    if existing.image {
        return Ok(Step::Teardown);
    }
    if existing.snapshot {
        return Ok(Step::CreateImage);
    }
    match existing.instance {
        // Stopped is not read as "finished" here; the watcher decides that, and on
        // a resume it decides it from the already-stopped state. Snapshotting is
        // simply the next step that has anything to do.
        Some(InstanceState::Stopped) => return Ok(Step::Snapshot),
        Some(_) => return Ok(Step::Watch),
        None => {}
    }
    match &existing.installer {
        None => Ok(Step::Upload),
        Some(DiskStatus::Usable) => Ok(Step::CreateInstance),
        // Everything else is either a half-finished import or a transient state.
        // `Creating`, `Attaching`, `Detaching` and `Maintenance` will pass on
        // their own and the run should be started again a moment later; the import
        // states and `Faulted` will not. Both are refused, because proceeding on a
        // disk that is mid-transition is guessing either way.
        Some(DiskStatus::Unusable(other)) => bail!(
            "the installer disk is in state `{other}`, which means a previous \
             upload did not finish or the disk is mid-transition. Nothing records \
             which bytes landed, so it cannot be resumed — re-uploading would \
             write onto unknown contents and could produce media that boots and \
             is subtly wrong.\n\nClear it and run this again:\n  \
             oxide disk import stop     --project <p> --disk <installer>\n  \
             oxide disk import finalize --project <p> --disk <installer>\n  \
             oxide disk delete          --project <p> --disk <installer>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide::types::{DiskState, InstanceState};

    fn attached() -> DiskStatus {
        DiskStatus::of(&DiskState::Attached(uuid::Uuid::nil()))
    }

    fn detached() -> DiskStatus {
        DiskStatus::of(&DiskState::Detached)
    }

    /// Both are finished disks. An attachment id is not the cycle's business.
    #[test]
    fn detached_and_attached_disks_are_both_usable() {
        assert_eq!(detached(), DiskStatus::Usable);
        assert_eq!(attached(), DiskStatus::Usable);
    }

    #[test]
    fn a_fresh_run_starts_at_the_upload() {
        assert_eq!(next_step(&Existing::default()).unwrap(), Step::Upload);
    }

    #[test]
    fn each_finished_step_is_skipped() {
        let mut e =
            Existing { installer: Some(detached()), ..Default::default() };
        assert_eq!(next_step(&e).unwrap(), Step::CreateInstance);

        e.system = Some(attached());
        e.instance = Some(InstanceState::Running);
        assert_eq!(next_step(&e).unwrap(), Step::Watch);

        e.instance = Some(InstanceState::Stopped);
        assert_eq!(next_step(&e).unwrap(), Step::Snapshot);

        e.snapshot = true;
        assert_eq!(next_step(&e).unwrap(), Step::CreateImage);

        e.image = true;
        assert_eq!(next_step(&e).unwrap(), Step::Teardown);
    }

    /// A stopped instance is not proof the install finished — the watcher makes
    /// that judgement, and on a resume it makes it from the already-stopped state.
    /// What matters here is only that we do not try to create an instance that
    /// exists.
    #[test]
    fn a_stopped_instance_goes_to_the_snapshot_not_to_creation() {
        let e = Existing {
            installer: Some(detached()),
            system: Some(attached()),
            instance: Some(InstanceState::Stopped),
            ..Default::default()
        };
        assert_eq!(next_step(&e).unwrap(), Step::Snapshot);
    }

    /// The one case that refuses. Nothing on the rack records which bytes of a
    /// half-uploaded disk landed, so re-uploading writes onto unknown contents and
    /// produces media that may boot and may be subtly wrong.
    #[test]
    fn a_half_uploaded_installer_refuses_rather_than_guessing() {
        for state in [
            DiskState::ImportingFromBulkWrites,
            DiskState::ImportReady,
            DiskState::Finalizing,
        ] {
            let status = DiskStatus::of(&state);
            assert!(
                matches!(status, DiskStatus::Unusable(_)),
                "{state:?} must not classify as usable"
            );
            let e = Existing { installer: Some(status), ..Default::default() };
            let error = next_step(&e).unwrap_err().to_string();
            assert!(
                error.contains("delete"),
                "the refusal has to say how to clear it: {error}"
            );
            // The refusal quotes the state, or nobody can tell a stuck import
            // apart from a disk that is merely mid-attach.
            assert!(error.contains(&format!("{state:?}")), "{error}");
        }
    }

    #[test]
    fn a_faulted_installer_refuses() {
        let e = Existing {
            installer: Some(DiskStatus::of(&DiskState::Faulted)),
            ..Default::default()
        };
        assert!(next_step(&e).is_err());
    }

    /// Resuming after the image exists is a teardown, not a second image.
    #[test]
    fn an_existing_image_means_only_teardown_is_left() {
        let e = Existing { image: true, ..Default::default() };
        assert_eq!(next_step(&e).unwrap(), Step::Teardown);
    }
}
