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

use crate::golden::{Names, Resource};
use anyhow::{Result, bail};
use oxide::types::{DiskState, InstanceState};

/// A disk, as far as this cycle is concerned.
///
/// The SDK's `DiskState` carries an attachment id and does not implement
/// `PartialEq`, and neither fact matters here: what the cycle needs to know is
/// whether a disk is finished enough to use. Classifying at the boundary keeps the
/// deciding code comparable and testable, and puts the judgment about which
/// states are usable in one place with a reason next to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskStatus {
    /// Detached or attached — a finished disk.
    Usable,
    /// Mid-import or mid-transition. Carries the state's name so the refusal can
    /// quote it back, and what gets the disk out of the way from that state.
    Unusable { state: String, clear: Clear },
}

/// How to get an unusable disk out of the way, which depends on how far its
/// import got. A disk refuses deletion while an import is open, so handing out a
/// bare `delete` for those states is a command that fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clear {
    /// Still taking bulk writes: stop the import, finalize it, then delete.
    StopFinalizeDelete,
    /// Stopped and never finalized: finalize, then delete.
    FinalizeDelete,
    /// Not going anywhere, but deletable as it is.
    Delete,
    /// Mid-transition, and should pass on its own. Try again; delete if it stays.
    Wait,
}

impl DiskStatus {
    pub fn of(state: &DiskState) -> Self {
        let clear = match state {
            DiskState::Detached | DiskState::Attached(_) => {
                return DiskStatus::Usable;
            }
            DiskState::ImportingFromBulkWrites => Clear::StopFinalizeDelete,
            DiskState::ImportReady => Clear::FinalizeDelete,
            DiskState::Faulted | DiskState::Destroyed => Clear::Delete,
            // `Finalizing` is here rather than with the imports: our upload only
            // finalizes after every byte was written, and it ends in `Detached`.
            DiskState::Creating
            | DiskState::Finalizing
            | DiskState::Maintenance
            | DiskState::ImportingFromUrl
            | DiskState::Attaching(_)
            | DiskState::Detaching(_) => Clear::Wait,
        };
        DiskStatus::Unusable { state: format!("{state:?}"), clear }
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

/// Decide the next step. `names` and `project` are only for the refusal: when
/// the installer disk is in the way, the error hands back the exact commands for
/// this run's disk rather than placeholders, because in the app the error modal is
/// the only place the user will see them.
pub fn next_step(
    existing: &Existing,
    names: &Names,
    project: &str,
) -> Result<Step> {
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
        // Both are refused, because proceeding on a disk that is mid-transition
        // is guessing either way.
        Some(DiskStatus::Unusable { state, clear }) => bail!(
            "{}",
            installer_refusal(state, *clear, &names.installer_disk(), project)
        ),
    }
}

/// The refusal for an installer disk that is in the way, with the commands that
/// clear it from the state it is in, filled in for this run.
fn installer_refusal(
    state: &str,
    clear: Clear,
    disk: &str,
    project: &str,
) -> String {
    let stop =
        format!("oxide disk import stop --project {project} --disk {disk}");
    let finalize =
        format!("oxide disk import finalize --project {project} --disk {disk}");
    let delete = Resource::Disk(disk.to_string()).delete_command(project);
    let (why, commands) = match clear {
        Clear::StopFinalizeDelete | Clear::FinalizeDelete => (
            "which means a previous upload did not finish. Nothing records \
             which bytes landed, so it cannot be resumed — re-uploading would \
             write onto unknown contents and could produce media that boots and \
             is subtly wrong. A disk mid-import refuses deletion, so the import \
             has to be closed first.\n\nClear it and run this again:",
            if clear == Clear::StopFinalizeDelete {
                vec![stop, finalize, delete]
            } else {
                vec![finalize, delete]
            },
        ),
        Clear::Delete => (
            "and will not recover on its own.\n\nDelete it and run this again:",
            vec![delete],
        ),
        Clear::Wait => (
            "which is mid-transition and should pass on its own. Run this again \
             in a moment.\n\nIf it is still in this state, delete it and run \
             this again:",
            vec![delete],
        ),
    };
    format!(
        "The installer disk {disk} is in state `{state}`, {why}\n  {}",
        commands.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide::types::{DiskState, InstanceState};

    /// `next_step` with a fixed run and project, for the tests that only care
    /// about the step.
    fn step(existing: &Existing) -> Result<Step> {
        next_step(existing, &Names::new("ws2022-golden").unwrap(), "danb")
    }

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
        assert_eq!(step(&Existing::default()).unwrap(), Step::Upload);
    }

    #[test]
    fn each_finished_step_is_skipped() {
        let mut e =
            Existing { installer: Some(detached()), ..Default::default() };
        assert_eq!(step(&e).unwrap(), Step::CreateInstance);

        e.system = Some(attached());
        e.instance = Some(InstanceState::Running);
        assert_eq!(step(&e).unwrap(), Step::Watch);

        e.instance = Some(InstanceState::Stopped);
        assert_eq!(step(&e).unwrap(), Step::Snapshot);

        e.snapshot = true;
        assert_eq!(step(&e).unwrap(), Step::CreateImage);

        e.image = true;
        assert_eq!(step(&e).unwrap(), Step::Teardown);
    }

    /// A stopped instance is not proof the install finished — the watcher makes
    /// that judgment, and on a resume it makes it from the already-stopped state.
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
        assert_eq!(step(&e).unwrap(), Step::Snapshot);
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
                matches!(status, DiskStatus::Unusable { .. }),
                "{state:?} must not classify as usable"
            );
            let e = Existing { installer: Some(status), ..Default::default() };
            let error = step(&e).unwrap_err().to_string();
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
        assert!(step(&e).is_err());
    }

    /// Resuming after the image exists is a teardown, not a second image.
    #[test]
    fn an_existing_image_means_only_teardown_is_left() {
        let e = Existing { image: true, ..Default::default() };
        assert_eq!(step(&e).unwrap(), Step::Teardown);
    }

    /// The refusal is the only place the app shows these commands, and the app
    /// knows the run and the project, so they are filled in: a `<p>` or a
    /// `<installer>` in a modal is a command the user has to rewrite by hand.
    /// And only the commands that work from this state, in the order they work.
    #[test]
    fn the_refusal_hands_back_this_runs_commands_for_this_state() {
        let stop = "oxide disk import stop --project danb \
                    --disk ws2022-golden-installer";
        let finalize = "oxide disk import finalize --project danb \
                        --disk ws2022-golden-installer";
        let delete =
            "oxide disk delete --project danb --disk ws2022-golden-installer";
        for (state, expected) in [
            (DiskState::ImportingFromBulkWrites, vec![stop, finalize, delete]),
            (DiskState::ImportReady, vec![finalize, delete]),
            (DiskState::Faulted, vec![delete]),
            (DiskState::Creating, vec![delete]),
            (DiskState::Finalizing, vec![delete]),
        ] {
            let e = Existing {
                installer: Some(DiskStatus::of(&state)),
                ..Default::default()
            };
            let error = step(&e).unwrap_err().to_string();
            assert!(!error.contains('<'), "a placeholder is left: {error}");
            let commands: Vec<&str> = error
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("oxide "))
                .collect();
            assert_eq!(commands, expected, "{state:?}: {error}");
        }
    }
}
