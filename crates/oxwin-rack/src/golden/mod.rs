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
pub use watch::{Action, Milestone, Watch, WatchOptions};

use crate::upload::{DiskSpec, Rack};
use anyhow::Result;
use oxwin_core::engine::Cancel;
use oxwin_core::progress::Reporter;

/// What one golden run needs to know.
#[derive(Debug, Clone)]
pub struct GoldenSpec {
    pub names: Names,
    /// The already-built image. The CLI resolves an ISO to one of these before
    /// calling in; this crate does not build.
    pub image_path: std::path::PathBuf,
    pub keep: Keep,
    pub watch: watch::WatchOptions,
    pub system_disk_gib: u64,
    pub ncpus: u16,
    pub memory_gib: u64,
    /// What the finished image reports about itself.
    pub os: String,
    pub version: String,
}

/// A finished run.
#[derive(Debug, Clone)]
pub struct Golden {
    pub image: String,
    /// What teardown could not remove. Not a failure — the image exists.
    pub leftovers: Vec<Resource>,
}

/// The one command that resumes a run, which is the command that was just run.
///
/// This is the payoff for reconciling against the rack instead of keeping a
/// journal: the recovery instruction after any failure is one line, and it is
/// always correct.
pub fn resume_hint(names: &Names, project: &str) -> String {
    format!(
        "to carry on where this stopped, run exactly the same command again:\n  \
         oxwin golden <source> --run={} --project={project}",
        names.run()
    )
}

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
            // An instance has to be stopped before it can be deleted, and its disks
            // cannot be deleted while it exists to hold them attached. So this is
            // not a nicety: skip it and the whole teardown fails, one resource at a
            // time, for a reason that reads like a rack fault.
            if let Resource::Instance(name) = &resource
                && let Err(e) = self.stop_and_wait(name, reporter)
            {
                reporter.log(format!("could not stop {name}: {e:#}"));
                stuck.push(resource);
                continue;
            }
            reporter.phase("teardown", format!("deleting {resource:?}"));
            match self.delete_with_retry(&resource, reporter) {
                Ok(()) => {}
                Err(e) => {
                    // `{e:#}` for the whole chain. Reporting only the outermost
                    // context produced "could not delete Disk(\"g4-system\"):
                    // deleting Disk(\"g4-system\")" on a rack -- the context
                    // repeated back with the actual reason discarded.
                    reporter
                        .log(format!("could not delete {resource:?}: {e:#}"));
                    stuck.push(resource);
                }
            }
        }
        Ok(stuck)
    }

    /// Delete, retrying briefly, because the first attempt races the rack.
    ///
    /// Deleting an instance detaches its disks *asynchronously*, so the disk deletes
    /// that follow can arrive while the disk is still attached and be refused. Seen
    /// on a rack: the system disk failed to delete and was `detached` and perfectly
    /// deletable moments later. The whole run had succeeded, so the only consequence
    /// was a resource left behind with a confusing explanation.
    ///
    /// Bounded and short: this is for a transient that clears in seconds, not a
    /// retry loop for a rack that is actually unwell.
    fn delete_with_retry(
        &self,
        resource: &Resource,
        reporter: &Reporter,
    ) -> Result<()> {
        const ATTEMPTS: usize = 5;
        const DELAY: std::time::Duration = std::time::Duration::from_secs(3);

        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            match self.delete(resource) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < ATTEMPTS {
                        reporter.log(format!(
                            "  {resource:?} not deletable yet (attempt \
                             {attempt}/{ATTEMPTS}); waiting"
                        ));
                        std::thread::sleep(DELAY);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.expect("a failure after every attempt"))
    }

    /// Build the golden image: upload, install, generalize, snapshot, image, tidy.
    ///
    /// Resumable by construction. Every pass round the loop asks the rack what
    /// already exists and does only the next thing that is missing, so running this
    /// twice with the same `--run` continues rather than collides — and that is why
    /// there is no journal.
    pub fn run_golden(
        &self,
        spec: &GoldenSpec,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> std::result::Result<Golden, crate::instance::Failure> {
        let names = &spec.names;
        let mut leftovers = crate::instance::Leftovers::default();
        // Guards against a step that "succeeds" without changing anything, which
        // would otherwise be an infinite loop against the control plane.
        let mut previous: Option<(Step, Existing)> = None;

        macro_rules! fail {
            ($e:expr) => {
                return Err(crate::instance::Failure {
                    error: $e,
                    leftovers: leftovers.clone(),
                })
            };
        }

        loop {
            if let Err(e) = cancel.check() {
                fail!(e);
            }

            let existing = match self.survey(names) {
                Ok(e) => e,
                Err(e) => fail!(e),
            };
            // Everything the survey found exists, so the failure report is accurate
            // whichever step fails next.
            leftovers = leftovers_from(names, &existing);

            let step = match next_step(&existing) {
                Ok(step) => step,
                Err(e) => fail!(e),
            };

            if previous.as_ref() == Some(&(step, existing.clone())) {
                fail!(anyhow::anyhow!(
                    "{step:?} ran but changed nothing on the rack. Stopping \
                     rather than retrying it forever"
                ));
            }
            previous = Some((step, existing.clone()));

            match step {
                Step::Upload => {
                    let disk = DiskSpec {
                        name: names.installer_disk(),
                        description: format!(
                            "Windows installer for golden run {}",
                            names.run()
                        ),
                        block_size: crate::upload::INSTALLER_BLOCK_SIZE,
                    };
                    if let Err(e) = self.upload_image(
                        &spec.image_path,
                        &disk,
                        reporter,
                        cancel,
                    ) {
                        fail!(e);
                    }
                }
                Step::CreateInstance => {
                    let mut instance = crate::InstanceSpec::for_installer(
                        &names.instance(),
                        &names.installer_disk(),
                    );
                    instance.system_disk = Some(names.system_disk());
                    instance.system_disk_gib = spec.system_disk_gib;
                    instance.ncpus = spec.ncpus;
                    instance.memory_gib = spec.memory_gib;
                    if let Err(mut failure) =
                        self.create_instance(&instance, reporter)
                    {
                        // Fold in what the survey already knew about, so the report
                        // is everything that exists rather than only what this step
                        // made.
                        failure.leftovers = leftovers.clone();
                        return Err(failure);
                    }
                }
                Step::Watch => {
                    if let Err(e) = self.watch_install(
                        &names.instance(),
                        &spec.watch,
                        reporter,
                        cancel,
                    ) {
                        fail!(e);
                    }
                }
                Step::Snapshot => {
                    if let Err(e) = self.snapshot_step(names, reporter) {
                        fail!(e);
                    }
                }
                Step::CreateImage => {
                    if let Err(e) = self.image_step(
                        names,
                        &spec.os,
                        &spec.version,
                        reporter,
                    ) {
                        fail!(e);
                    }
                }
                Step::Teardown => {
                    let stuck =
                        match self.teardown_step(names, spec.keep, reporter) {
                            Ok(stuck) => stuck,
                            Err(e) => fail!(e),
                        };
                    return Ok(Golden {
                        image: names.image(),
                        leftovers: stuck,
                    });
                }
            }
        }
    }

    /// What this run's names currently point at.
    fn survey(&self, names: &Names) -> Result<Existing> {
        Ok(Existing {
            installer: self
                .disk_state(&names.installer_disk())?
                .as_ref()
                .map(DiskStatus::of),
            system: self
                .disk_state(&names.system_disk())?
                .as_ref()
                .map(DiskStatus::of),
            instance: self.instance_state(&names.instance())?,
            snapshot: self.snapshot_id(&names.snapshot())?.is_some(),
            image: self.image_exists(&names.image())?,
        })
    }
}

/// Everything that exists, named, so a failure can list it.
fn leftovers_from(
    names: &Names,
    existing: &Existing,
) -> crate::instance::Leftovers {
    let mut l = crate::instance::Leftovers::default();
    if existing.instance.is_some() {
        l.instances.push(names.instance());
    }
    if existing.installer.is_some() {
        l.disks.push(names.installer_disk());
    }
    if existing.system.is_some() {
        l.disks.push(names.system_disk());
    }
    if existing.snapshot {
        l.snapshots.push(names.snapshot());
    }
    if existing.image {
        l.images.push(names.image());
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Teardown must never be reachable from a failure path. The image is the
    /// product of an hour, and a transient error must not discard the work.
    #[test]
    fn teardown_is_only_called_after_the_image_exists() {
        let source = include_str!("mod.rs");
        let run = source
            .split("pub fn run_golden(")
            .nth(1)
            .expect("the sequencer is missing");
        let image_at =
            run.find("image_step").expect("no image step in the sequencer");
        let teardown_at =
            run.find("teardown_step").expect("no teardown in the sequencer");
        assert!(
            image_at < teardown_at,
            "teardown must come after the image is made, not before"
        );
    }

    /// Every failure has to hand back the one command that resumes, and that
    /// command is the command they just ran. It is the payoff for reconciling
    /// instead of journalling, and worth pinning so nobody replaces it with a list
    /// of manual steps later.
    #[test]
    fn the_failure_message_names_the_resume_command() {
        let names = Names::new("g").unwrap();
        let text = resume_hint(&names, "danb");
        assert!(text.contains("oxwin golden"), "{text}");
        assert!(text.contains("--run=g"), "{text}");
        assert!(text.contains("--project=danb"), "{text}");
    }

    /// A failure has to name everything that exists, not only what the failing
    /// step made. Otherwise a run that dies during the watch reports no leftovers
    /// at all, and the user is told nothing about the two disks and the instance
    /// sitting on their rack.
    #[test]
    fn leftovers_name_everything_the_survey_found() {
        let names = Names::new("g").unwrap();
        let all = Existing {
            installer: Some(DiskStatus::Usable),
            system: Some(DiskStatus::Usable),
            instance: Some(oxide::types::InstanceState::Stopped),
            snapshot: true,
            image: true,
        };
        let l = leftovers_from(&names, &all);
        assert_eq!(l.instances, vec!["g"]);
        assert_eq!(l.disks, vec!["g-installer", "g-system"]);
        assert_eq!(l.snapshots, vec!["g-snap"]);
        assert_eq!(l.images, vec!["g"]);

        assert!(leftovers_from(&names, &Existing::default()).is_empty());
    }

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
