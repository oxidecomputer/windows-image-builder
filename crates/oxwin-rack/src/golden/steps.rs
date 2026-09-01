// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The rack calls a golden run makes.
//!
//! Deliberately thin: no decisions live here, because nothing here can be tested
//! without a rack. What comes back is normalised so the deciding code
//! ([`super::watch`], [`super::reconcile`]) sees plain values — chiefly, **a
//! resource that does not exist is `None`, not an error**, since "not there yet" is
//! the normal state of every one of these during a run.

use crate::golden::keep::Resource;
use crate::golden::names::Names;
use crate::golden::watch::{Action, Milestone, Watch, WatchOptions};
use crate::upload::Rack;
use anyhow::{Context, Result};
use oxide::types::{ExternalIp, InstanceState};
use oxide::{
    ClientDisksExt, ClientImagesExt, ClientInstancesExt, ClientSnapshotsExt,
};
use oxwin_core::engine::Cancel;
use oxwin_core::progress::Reporter;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

/// The address to poll for port 22, or `None` if there is nothing reachable.
///
/// **A SNAT address is not one.** SNAT is outbound connectivity only; nothing can
/// connect *to* it. Polling one would never answer, and the watcher would report a
/// failed install on a machine that installed perfectly — which is exactly the
/// class of silent wrongness this project keeps finding.
pub fn pick_address(ips: &[ExternalIp]) -> Option<IpAddr> {
    ips.iter().find_map(|ip| match ip {
        ExternalIp::Ephemeral { ip, .. } => Some(*ip),
        ExternalIp::Floating { ip, .. } => Some(*ip),
        ExternalIp::Snat { .. } => None,
    })
}

/// The longest a single port probe may block the watch loop.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Is anything listening?
///
/// A refused or timed-out connection is `false`, not an error: for most of an
/// install that is the correct and expected answer, and an error here would end the
/// watch on a machine that is working.
pub fn port_open(addr: IpAddr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(addr, port), timeout).is_ok()
}

/// True when an SDK error is a 404, so "no such resource" is not a failure.
///
/// Compared as a number rather than against `reqwest::StatusCode`, so this crate
/// does not take a dependency on the HTTP stack for one constant.
fn is_not_found<E>(error: &oxide::Error<E>) -> bool {
    error.status().is_some_and(|code| code.as_u16() == 404)
}

/// Run a delete, treating "no such thing" as success.
///
/// That is what makes teardown re-runnable after a partial failure: already gone
/// and never created have to look the same, or a second attempt fails on the
/// resource the first one removed.
fn deleted<T, E: std::fmt::Debug>(
    result: std::result::Result<T, oxide::Error<E>>,
) -> Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

impl Rack {
    /// The instance's state, or `None` if there is no such instance.
    pub fn instance_state(&self, name: &str) -> Result<Option<InstanceState>> {
        match self.block_on(
            self.client()
                .instance_view()
                .project(self.project())
                .instance(name)
                .send(),
        ) {
            Ok(view) => Ok(Some(view.into_inner().run_state)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("reading instance {name}")),
        }
    }

    pub fn disk_state(
        &self,
        name: &str,
    ) -> Result<Option<oxide::types::DiskState>> {
        match self.block_on(
            self.client().disk_view().project(self.project()).disk(name).send(),
        ) {
            Ok(view) => Ok(Some(view.into_inner().state)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("reading disk {name}")),
        }
    }

    /// The snapshot's id **and its state**, or `None` if there is no such snapshot.
    ///
    /// The state matters: a snapshot exists as `creating` before it is `ready`, and
    /// an id alone says nothing about which. Handing a `creating` snapshot to
    /// `image_create` is a bug that would only appear on a rack slow enough to be
    /// caught mid-flight.
    pub fn snapshot_status(
        &self,
        name: &str,
    ) -> Result<Option<(uuid::Uuid, oxide::types::SnapshotState)>> {
        match self.block_on(
            self.client()
                .snapshot_view()
                .project(self.project())
                .snapshot(name)
                .send(),
        ) {
            Ok(view) => {
                let snapshot = view.into_inner();
                Ok(Some((snapshot.id, snapshot.state)))
            }
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("reading snapshot {name}")),
        }
    }

    /// The snapshot's id, which is what `image_create` wants, or `None`.
    pub fn snapshot_id(&self, name: &str) -> Result<Option<uuid::Uuid>> {
        match self.block_on(
            self.client()
                .snapshot_view()
                .project(self.project())
                .snapshot(name)
                .send(),
        ) {
            Ok(view) => Ok(Some(view.into_inner().id)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("reading snapshot {name}")),
        }
    }

    /// The image's id, or `None`. One call rather than two, so
    /// [`Self::image_exists`] and the clone's disk source share it.
    pub fn image_id(&self, name: &str) -> Result<Option<uuid::Uuid>> {
        match self.block_on(
            self.client()
                .image_view()
                .project(self.project())
                .image(name)
                .send(),
        ) {
            Ok(view) => Ok(Some(view.into_inner().id)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("reading image {name}")),
        }
    }

    pub fn image_exists(&self, name: &str) -> Result<bool> {
        Ok(self.image_id(name)?.is_some())
    }

    /// Where to poll for port 22.
    pub fn reachable_address(&self, instance: &str) -> Result<Option<IpAddr>> {
        let ips = self
            .block_on(
                self.client()
                    .instance_external_ip_list()
                    .project(self.project())
                    .instance(instance)
                    .send(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("listing external IPs of {instance}"))?
            .into_inner()
            .items;
        Ok(pick_address(&ips))
    }

    /// The last `bytes` of what the guest wrote to COM1.
    ///
    /// Diagnosis only, never a signal: desktop editions write nothing here. It is
    /// fetched after something has already gone wrong, because the project's rule
    /// is to read what the machine wrote down before reasoning about what Windows
    /// does.
    pub fn serial_tail(&self, instance: &str, bytes: u64) -> Result<String> {
        let data = self
            .block_on(
                self.client()
                    .instance_serial_console()
                    .project(self.project())
                    .instance(instance)
                    .most_recent(bytes)
                    .send(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| {
                format!("reading the serial console of {instance}")
            })?
            .into_inner()
            .data;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    /// Snapshot the system disk. Returns the id `image_create` needs.
    pub fn create_snapshot(
        &self,
        disk: &str,
        snapshot: &str,
    ) -> Result<uuid::Uuid> {
        let body = oxide::types::SnapshotCreate {
            name: snapshot.parse().map_err(|e| {
                anyhow::anyhow!("snapshot name {snapshot:?}: {e}")
            })?,
            description: format!("Generalized Windows, from {disk}"),
            disk: disk
                .parse::<oxide::types::NameOrId>()
                .map_err(|e| anyhow::anyhow!("disk name {disk:?}: {e}"))?,
        };
        let created = self
            .block_on(
                self.client()
                    .snapshot_create()
                    .project(self.project())
                    .body(body)
                    .send(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("snapshotting {disk} as {snapshot}"))?;
        Ok(created.into_inner().id)
    }

    /// The product: an image, from the snapshot.
    pub fn create_image(
        &self,
        name: &str,
        snapshot: uuid::Uuid,
        os: &str,
        version: &str,
    ) -> Result<()> {
        let body = oxide::types::ImageCreate {
            name: name
                .parse()
                .map_err(|e| anyhow::anyhow!("image name {name:?}: {e}"))?,
            description: format!("Generalized {os} {version}, built by oxwin"),
            os: os.to_string(),
            version: version.to_string(),
            source: oxide::types::ImageSource::Snapshot(snapshot),
        };
        self.block_on(
            self.client()
                .image_create()
                .project(self.project())
                .body(body)
                .send(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("creating image {name}"))?;
        Ok(())
    }

    /// Stop an instance and wait until the control plane agrees it is stopped.
    ///
    /// **A running instance cannot be deleted**, and stopping is not instant, so a
    /// teardown that goes straight to the delete fails with "instance is running or
    /// has not yet fully stopped" and then cannot remove the disks either, because
    /// they are still attached to the instance that would not die.
    ///
    /// This was invisible in the golden cycle, where the guest shuts *itself* down
    /// and the instance is already stopped by the time teardown runs. It is not
    /// invisible for `--verify-clone`, which deliberately leaves a clone running.
    ///
    /// Already stopped, or already gone, is success.
    pub fn stop_and_wait(
        &self,
        instance: &str,
        reporter: &Reporter,
    ) -> Result<()> {
        const ATTEMPTS: usize = 40;
        const DELAY: Duration = Duration::from_secs(3);

        match self.instance_state(instance)? {
            None | Some(InstanceState::Stopped) => return Ok(()),
            Some(_) => {}
        }

        reporter.phase("teardown", format!("stopping {instance}"));
        // A refusal here is not fatal on its own: it may already be stopping, and
        // the poll below is what actually decides.
        let _ = self.block_on(
            self.client()
                .instance_stop()
                .project(self.project())
                .instance(instance)
                .send(),
        );

        for _ in 0..ATTEMPTS {
            match self.instance_state(instance)? {
                None | Some(InstanceState::Stopped) => return Ok(()),
                Some(_) => std::thread::sleep(DELAY),
            }
        }
        anyhow::bail!(
            "{instance} did not stop within {}s, so it cannot be deleted and \
             neither can its disks",
            ATTEMPTS as u64 * DELAY.as_secs()
        )
    }

    /// A disk whose contents are a finished image. The clone's system disk.
    pub fn create_disk_from_image(
        &self,
        disk: &str,
        image: &str,
    ) -> Result<()> {
        let image_id = self.image_id(image)?.ok_or_else(|| {
            anyhow::anyhow!("there is no image called {image}")
        })?;
        let size = self
            .block_on(
                self.client()
                    .image_view()
                    .project(self.project())
                    .image(image)
                    .send(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .into_inner()
            .size;
        let body = oxide::types::DiskCreate {
            name: disk
                .parse()
                .map_err(|e| anyhow::anyhow!("disk name {disk:?}: {e}"))?,
            description: format!("Clone of {image}, to prove it boots"),
            size,
            // Writable: the clone boots from it and runs OOBE, which writes.
            disk_backend: oxide::types::DiskSource::Image {
                image_id,
                read_only: false,
            }
            .into(),
        };
        self.block_on(
            self.client()
                .disk_create()
                .project(self.project())
                .body(body)
                .send(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("creating {disk} from image {image}"))?;
        Ok(())
    }

    /// Prove the image boots: a disk from it, an instance from that disk, and then
    /// wait for it to come up **and stay up**.
    ///
    /// Idempotent like everything else, so a re-run reuses a clone that already
    /// exists rather than colliding with its name.
    ///
    /// Until this has run and been checked, "golden image" is a claim rather than a
    /// feature. What it cannot check is the part that matters most — that the
    /// clone's computer name and SID differ from the machine the image came from —
    /// because that needs a look inside the guest. The caller says so.
    pub fn verify_clone(
        &self,
        names: &Names,
        opts: &WatchOptions,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> Result<()> {
        if self.instance_state(&names.clone_instance())?.is_none() {
            reporter.phase(
                "verify",
                format!(
                    "creating {} from image {}",
                    names.clone_instance(),
                    names.image()
                ),
            );
            if self.disk_state(&names.clone_disk())?.is_none() {
                self.create_disk_from_image(
                    &names.clone_disk(),
                    &names.image(),
                )?;
            }
            let mut spec = crate::InstanceSpec::for_installer(
                &names.clone_instance(),
                &names.clone_disk(),
            );
            // The image is the system disk. A clone needs no second one.
            spec.system_disk = None;
            self.create_instance(&spec, reporter).map_err(|f| f.error)?;
        }
        reporter.phase(
            "verify",
            format!(
                "waiting for {} to come up and stay up",
                names.clone_instance()
            ),
        );
        self.watch_for(
            &names.clone_instance(),
            Watch::for_clone(opts.timeout),
            opts,
            reporter,
            cancel,
        )
    }

    /// Remove one resource. **Already gone is success**, which is what makes
    /// teardown re-runnable after a partial failure.
    pub fn delete(&self, resource: &Resource) -> Result<()> {
        let project = self.project();
        let outcome = match resource {
            Resource::Instance(name) => deleted(
                self.block_on(
                    self.client()
                        .instance_delete()
                        .project(project)
                        .instance(name)
                        .send(),
                ),
            ),
            Resource::Disk(name) => deleted(self.block_on(
                self.client().disk_delete().project(project).disk(name).send(),
            )),
            Resource::Snapshot(name) => deleted(
                self.block_on(
                    self.client()
                        .snapshot_delete()
                        .project(project)
                        .snapshot(name)
                        .send(),
                ),
            ),
            Resource::Image(name) => deleted(
                self.block_on(
                    self.client()
                        .image_delete()
                        .project(project)
                        .image(name)
                        .send(),
                ),
            ),
        };
        outcome.with_context(|| format!("deleting {resource:?}"))
    }

    /// Wait for a golden install to finish and shut itself down.
    ///
    /// Returns once the instance has stopped *having been reachable* — see
    /// [`super::watch`] for why that qualification is the whole point. On a failure
    /// or a timeout the serial console tail is emitted as log lines first, because
    /// the project's rule is to read what the machine wrote down before reasoning
    /// about what Windows does.
    pub fn watch_install(
        &self,
        instance: &str,
        opts: &WatchOptions,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> Result<()> {
        reporter.phase(
            "watch",
            format!("waiting for {instance} to install and generalize"),
        );
        self.watch_for(
            instance,
            Watch::new(opts.timeout),
            opts,
            reporter,
            cancel,
        )
    }

    /// The loop itself, shared by the golden watch and the clone check.
    ///
    /// They differ only in the [`Watch`] they start with — the two have opposite
    /// finish conditions — so the loop is written once.
    pub(crate) fn watch_for(
        &self,
        instance: &str,
        mut watch: Watch,
        opts: &WatchOptions,
        reporter: &Reporter,
        cancel: &Cancel,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        // Looked up once it exists, then cached: an instance has its address from
        // creation, and re-listing every fifteen seconds buys nothing.
        let mut address: Option<IpAddr> = None;
        // The tick exists so an hour-long wait does not read as a hang, and that
        // needs far less than one line per poll. Measured on a real install: 88
        // lines in 22 minutes, which over a full timeout is around 480 — enough
        // that the milestones scroll away among them. A state change always
        // prints; otherwise once a minute.
        let mut last_tick: Option<std::time::Instant> = None;
        let mut last_state: Option<InstanceState> = None;

        loop {
            cancel.check()?;

            let state = self.instance_state(instance)?.ok_or_else(|| {
                anyhow::anyhow!("there is no instance called {instance}")
            })?;
            if address.is_none() {
                address = self.reachable_address(instance)?;
            }
            let port_22 = match address {
                // Capped as well as bounded by the poll interval. A probe that
                // blocks for the whole interval turns a fifteen-second loop into a
                // thirty-second one, and it is also time the loop cannot notice a
                // Ctrl-C in. Three seconds is many times a TCP handshake.
                Some(addr) => {
                    port_open(addr, 22, (opts.poll / 2).min(PROBE_TIMEOUT))
                }
                None => false,
            };

            match watch.observe(state, port_22, started.elapsed()) {
                Action::KeepWaiting => {}
                // Deliberately not "Windows Setup has begun": the same loop runs
                // the clone check, where this is OOBE on an already-installed
                // machine and saying Setup would be a lie in the log.
                Action::Reached(Milestone::Running) => reporter.phase(
                    "watch",
                    format!(
                        "{instance} is running; the guest has begun booting"
                    ),
                ),
                // Neutral for the same reason as the milestone above: on a clone
                // this is OOBE finishing, not Setup, and the log should not say
                // otherwise. It is also weaker than it looks -- sshd starts
                // automatically and answers while OOBE is still running.
                Action::Reached(Milestone::Reachable) => reporter.phase(
                    "watch",
                    format!(
                        "{instance} answered on port 22 after {}",
                        elapsed(started)
                    ),
                ),
                Action::Finished => {
                    reporter.phase(
                        "watch",
                        format!(
                            "{instance} finished after {}",
                            elapsed(started)
                        ),
                    );
                    return Ok(());
                }
                Action::Failed(why) => {
                    self.report_serial(instance, opts, reporter);
                    anyhow::bail!("{why}");
                }
            }

            // Progress that moves, so an hour-long wait does not read as a hang —
            // but not on every poll. A state change always prints; otherwise once
            // a minute.
            let changed = last_state != Some(state);
            let due = last_tick
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
            if changed || due {
                reporter.log(format!(
                    "  {instance}: {state} after {}",
                    elapsed(started)
                ));
                last_tick = Some(std::time::Instant::now());
            }
            last_state = Some(state);
            sleep_until_cancelled(opts.poll, cancel);
        }
    }

    /// Emit the serial tail as log lines.
    ///
    /// A failure to read it is itself only a log line: we are already reporting a
    /// failure, and losing the diagnosis must not replace the diagnosis.
    fn report_serial(
        &self,
        instance: &str,
        opts: &WatchOptions,
        reporter: &Reporter,
    ) {
        match self.serial_tail(instance, opts.serial_bytes) {
            Ok(text) if text.trim().is_empty() => reporter.log(
                "the serial console is empty. That is normal on Windows 10 and \
                 11, which write nothing to COM1"
                    .to_string(),
            ),
            Ok(text) => {
                reporter
                    .log(format!("last {} bytes of COM1:", opts.serial_bytes));
                for line in text.lines() {
                    reporter.log(format!("  | {line}"));
                }
            }
            Err(e) => {
                reporter.log(format!("could not read the serial console: {e}"))
            }
        }
    }
}

/// Wait, but wake up promptly when cancelled.
///
/// A plain `sleep(poll)` makes Ctrl-C take up to a full poll interval to be
/// noticed, and during a watch there is nothing to tear down — the guest is
/// installing and does not care — so a cancel there should be immediate. Someone
/// who presses Ctrl-C and sees nothing happen for twenty seconds presses it again,
/// and the second press is the one that abandons rather than cleans up.
fn sleep_until_cancelled(total: Duration, cancel: &Cancel) {
    const SLICE: Duration = Duration::from_millis(200);
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if cancel.is_cancelled() {
            return;
        }
        std::thread::sleep(SLICE.min(deadline - std::time::Instant::now()));
    }
}

fn elapsed(started: std::time::Instant) -> String {
    let s = started.elapsed().as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn snat(ip: IpAddr) -> oxide::types::ExternalIp {
        oxide::types::ExternalIp::Snat {
            first_port: 0,
            ip,
            ip_pool_id: uuid::Uuid::nil(),
            last_port: 16383,
        }
    }

    /// A SNAT address is outbound only. Polling one would never connect, and the
    /// watcher would report a failed install on a machine that installed perfectly.
    #[test]
    fn snat_addresses_are_not_reachable_addresses() {
        let ips = vec![
            snat(v4(10, 0, 0, 1)),
            oxide::types::ExternalIp::Ephemeral {
                ip: v4(192, 168, 1, 5),
                ip_pool_id: uuid::Uuid::nil(),
            },
        ];
        assert_eq!(pick_address(&ips), Some(v4(192, 168, 1, 5)));
    }

    #[test]
    fn an_instance_with_only_snat_has_no_address_to_poll() {
        assert_eq!(pick_address(&[snat(v4(10, 0, 0, 1))]), None);
    }

    /// During a watch there is nothing to tear down, so a cancel has to be
    /// immediate. It was not: the loop checked only at the top and then slept a
    /// whole poll interval, so Ctrl-C took up to twenty seconds to be noticed —
    /// long enough that the natural response is to press it again, and the second
    /// press abandons rather than cleans up. Found on a rack, not in a test.
    #[test]
    fn a_cancelled_sleep_returns_at_once() {
        let cancel = Cancel::new();
        cancel.cancel();
        let started = std::time::Instant::now();
        sleep_until_cancelled(Duration::from_secs(30), &cancel);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a cancelled sleep waited {:?}",
            started.elapsed()
        );
    }

    /// And an uncancelled one still waits.
    #[test]
    fn an_uncancelled_sleep_waits_its_full_time() {
        let started = std::time::Instant::now();
        sleep_until_cancelled(Duration::from_millis(500), &Cancel::new());
        assert!(started.elapsed() >= Duration::from_millis(450));
    }

    /// Closed is the normal case for most of an install — it must be an answer, not
    /// an error that ends the watch.
    #[test]
    fn a_closed_port_is_a_false_not_a_failure() {
        // Port 1 on the loopback: nothing listens, and the connection is refused
        // immediately rather than timing out.
        assert!(!port_open(
            v4(127, 0, 0, 1),
            1,
            std::time::Duration::from_millis(200)
        ));
    }
}
