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

        loop {
            cancel.check()?;

            let state = self.instance_state(instance)?.ok_or_else(|| {
                anyhow::anyhow!("there is no instance called {instance}")
            })?;
            if address.is_none() {
                address = self.reachable_address(instance)?;
            }
            let port_22 = match address {
                // Half the poll interval: a probe that blocks for the whole
                // interval turns a fifteen-second loop into a thirty-second one.
                Some(addr) => port_open(addr, 22, opts.poll / 2),
                None => false,
            };

            match watch.observe(state, port_22, started.elapsed()) {
                Action::KeepWaiting => {}
                Action::Reached(Milestone::Running) => reporter.phase(
                    "watch",
                    format!("{instance} is running; Windows Setup has begun"),
                ),
                Action::Reached(Milestone::Reachable) => reporter.phase(
                    "watch",
                    format!(
                        "{instance} answered on port 22 after {}: Setup has \
                         finished",
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

            // Progress that moves, so an hour-long wait does not read as a hang.
            reporter.log(format!(
                "  {instance}: {state} after {}",
                elapsed(started)
            ));
            std::thread::sleep(opts.poll);
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
