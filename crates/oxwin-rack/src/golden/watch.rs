// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Knowing when an install has finished, without being able to see the guest.
//!
//! # Why the control plane, and not the serial console
//!
//! A golden build ends by shutting itself down: `OxideGeneralize` syspreps and
//! powers off. **A guest shutdown transitions the instance to `Stopped`; a guest
//! reboot does not** — and Windows Setup reboots several times while the instance
//! stays `Running`. So `Stopped` is the tell, it works on every release, and it
//! needs no external IP and no firewall rule.
//!
//! A serial-console watcher would not: Windows desktop editions write nothing to
//! COM1, so anything keyed on serial output works on server media and hangs forever
//! on Windows 10 and 11 — this project's characteristic failure, built on purpose.
//!
//! # Why port 22 as well
//!
//! For one judgement: **an instance that reaches `Stopped` without port 22 ever
//! having answered did not finish.** A guest that dies during Setup also stops the
//! machine. Without that discrimination, a failed install and a finished golden
//! image are the same observation.
//!
//! Port 22 is already open in the default VPC, which is why it and not 3389.
//!
//! Everything here is a pure function of what was observed. The polling loop lives
//! in [`super::steps`]; this is the part that decides, and it is the part a rack
//! test would exercise once per hour.

use oxide::types::InstanceState;
use std::time::Duration;

/// Something worth telling the user, announced once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Milestone {
    /// The instance started.
    Running,
    /// Port 22 answered: Setup has finished and the guest can be logged into.
    Reachable,
}

/// What the caller should do with an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    KeepWaiting,
    Reached(Milestone),
    /// Generalized and powered off. The system disk is ready to snapshot.
    Finished,
    Failed(String),
}

/// The watcher's memory: which milestones have been announced, and what was seen.
pub struct Watch {
    timeout: Duration,
    announced_running: bool,
    /// Whether the Reachable milestone has been emitted. Distinct from
    /// `ever_reachable`, which is the judgement input and is never reset.
    reachable_announced: bool,
    /// Sticky. SSH answering and then going away is what a reboot looks like from
    /// outside, and un-setting this would make the following `Stopped` read as a
    /// failed install.
    ever_reachable: bool,
    /// Whether this watch ever saw the instance running. False means it was already
    /// stopped when watching began — a resume, not a failure.
    ever_running: bool,
}

impl Watch {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            announced_running: false,
            reachable_announced: false,
            ever_reachable: false,
            ever_running: false,
        }
    }

    pub fn observe(
        &mut self,
        state: InstanceState,
        port_22: bool,
        elapsed: Duration,
    ) -> Action {
        if port_22 {
            self.ever_reachable = true;
        }
        if matches!(state, InstanceState::Running) {
            self.ever_running = true;
        }

        match state {
            // Checked before the timeout: a finish arriving on the same poll as the
            // deadline is a finish. Reporting a timeout on a completed install
            // sends someone hunting a bug that is not there.
            InstanceState::Stopped => return self.stopped(),
            InstanceState::Failed => {
                return Action::Failed(
                    "the instance is in the failed state. Nothing in the guest \
                     caused this; check the rack"
                        .into(),
                );
            }
            InstanceState::Destroyed => {
                return Action::Failed(
                    "the instance was destroyed while we were watching it"
                        .into(),
                );
            }
            _ => {}
        }

        if elapsed >= self.timeout {
            return Action::Failed(self.timeout_reason());
        }

        if matches!(state, InstanceState::Running) && !self.announced_running {
            self.announced_running = true;
            return Action::Reached(Milestone::Running);
        }
        if port_22 && !self.reachable_announced {
            self.reachable_announced = true;
            return Action::Reached(Milestone::Reachable);
        }
        Action::KeepWaiting
    }

    fn stopped(&self) -> Action {
        // Already stopped when watching began: a resume of a run that finished, not
        // a failure. There was never a chance to see port 22 answer.
        if !self.ever_running && !self.announced_running {
            return Action::Finished;
        }
        if !self.ever_reachable {
            return Action::Failed(
                "the instance stopped, but port 22 never answered, so Setup \
                 never finished. A guest that dies during Setup also stops the \
                 machine. The serial console tail below is the place to start"
                    .into(),
            );
        }
        Action::Finished
    }

    fn timeout_reason(&self) -> String {
        if self.ever_reachable {
            // The install worked. So either the guest is not a golden build, or
            // the task did not run.
            "timed out after Setup had finished. The machine installed and came \
             up, but never shut itself down — which is what media built without \
             `generalize` does, since OxideGeneralize was never registered. Check \
             C:\\oxide-bootstrap.log in the guest for whether it registered"
                .to_string()
        } else {
            "timed out before port 22 ever answered, so Setup never finished. \
             The serial console tail below is the place to start"
                .to_string()
        }
    }
}

/// How long to wait, and how often to look.
#[derive(Debug, Clone, Copy)]
pub struct WatchOptions {
    /// Server 2022 reaches a login prompt in tens of minutes across several
    /// reboots, and 2025 and 11 are slower. An hour is too tight to be a safe
    /// default; two hours is well past anything observed and still finite, which is
    /// what makes this usable from a script.
    pub timeout: Duration,
    /// Coarse on purpose. Every 15 s is plenty across an hour-long wait, and a
    /// tighter loop against a control plane is rude for no benefit.
    pub poll: Duration,
    /// How much serial console to fetch when something has gone wrong.
    pub serial_bytes: u64,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2 * 60 * 60),
            poll: Duration::from_secs(15),
            serial_bytes: 16 * 1024,
        }
    }
}

/// `90s`, `30m`, `2h`, or a bare number of seconds.
pub fn parse_duration(raw: &str) -> anyhow::Result<Duration> {
    let (value, multiplier) = match raw.chars().last() {
        Some('s') => (&raw[..raw.len() - 1], 1),
        Some('m') => (&raw[..raw.len() - 1], 60),
        Some('h') => (&raw[..raw.len() - 1], 3600),
        Some(c) if c.is_ascii_digit() => (raw, 1),
        _ => anyhow::bail!("{raw:?}: expected a duration like 90s, 30m or 2h"),
    };
    let n: u64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("{raw:?}: expected a duration like 2h"))?;
    Ok(Duration::from_secs(n * multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide::types::InstanceState as S;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

    fn watch() -> Watch {
        Watch::new(TIMEOUT)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// The happy path, in order: it starts, Setup finishes and the guest answers on
    /// 22, then OxideGeneralize syspreps and shuts the machine down.
    #[test]
    fn the_three_milestones_in_order() {
        let mut w = watch();
        assert_eq!(
            w.observe(S::Starting, false, secs(10)),
            Action::KeepWaiting
        );
        assert_eq!(
            w.observe(S::Running, false, secs(20)),
            Action::Reached(Milestone::Running)
        );
        // A milestone is announced once, not on every poll.
        assert_eq!(w.observe(S::Running, false, secs(30)), Action::KeepWaiting);
        assert_eq!(
            w.observe(S::Running, true, secs(900)),
            Action::Reached(Milestone::Reachable)
        );
        assert_eq!(w.observe(S::Running, true, secs(910)), Action::KeepWaiting);
        assert_eq!(w.observe(S::Stopped, false, secs(1800)), Action::Finished);
    }

    /// The judgement the whole watcher exists to make. A guest that dies during
    /// Setup also stops the machine, so `stopped` alone cannot mean "finished" —
    /// and the wrong answer here costs an hour and produces an image of a broken
    /// install.
    #[test]
    fn stopping_without_ever_answering_on_22_is_a_failed_install() {
        let mut w = watch();
        w.observe(S::Running, false, secs(20));
        let action = w.observe(S::Stopped, false, secs(600));
        let Action::Failed(why) = action else {
            panic!("expected a failure, got {action:?}");
        };
        assert!(
            why.contains("22"),
            "the reason must say what was never seen: {why}"
        );
    }

    /// Windows Setup reboots several times. A guest reboot leaves the instance
    /// running, and even the explicit `Rebooting` state is not the end of anything.
    #[test]
    fn rebooting_is_not_finishing() {
        let mut w = watch();
        w.observe(S::Running, false, secs(20));
        assert_eq!(
            w.observe(S::Rebooting, false, secs(300)),
            Action::KeepWaiting
        );
        assert_eq!(
            w.observe(S::Running, false, secs(360)),
            Action::KeepWaiting
        );
    }

    /// SSH answering and then going away is exactly what a reboot looks like from
    /// outside. It must not un-set the milestone, or a later `stopped` would be
    /// misread as a failed install.
    #[test]
    fn reachability_is_remembered_once_seen() {
        let mut w = watch();
        w.observe(S::Running, false, secs(20));
        w.observe(S::Running, true, secs(900));
        assert_eq!(
            w.observe(S::Running, false, secs(950)),
            Action::KeepWaiting
        );
        assert_eq!(w.observe(S::Stopped, false, secs(1200)), Action::Finished);
    }

    /// Resume: the instance is already stopped when the watch starts. It never saw
    /// port 22 answer, but it also never had the chance, so this must not be read
    /// as a failed install — otherwise resuming a completed run reports a failure.
    #[test]
    fn an_instance_already_stopped_when_watching_begins_is_finished() {
        let mut w = watch();
        assert_eq!(w.observe(S::Stopped, false, secs(0)), Action::Finished);
    }

    #[test]
    fn a_failed_instance_fails_immediately() {
        let mut w = watch();
        let action = w.observe(S::Failed, false, secs(100));
        assert!(matches!(action, Action::Failed(_)), "{action:?}");
    }

    /// The timeout has to name the most likely cause. Media built without
    /// `generalize` installs perfectly and never stops, and the watcher cannot tell
    /// that apart from a hang.
    #[test]
    fn the_timeout_names_generalize_as_the_first_suspect() {
        let mut w = watch();
        w.observe(S::Running, false, secs(20));
        w.observe(S::Running, true, secs(900));
        let action = w.observe(S::Running, true, TIMEOUT + secs(1));
        let Action::Failed(why) = action else {
            panic!("expected a timeout, got {action:?}");
        };
        assert!(why.contains("generalize"), "{why}");
    }

    #[test]
    fn the_defaults_are_the_ones_the_spec_argued_for() {
        let o = WatchOptions::default();
        assert_eq!(o.timeout, Duration::from_secs(2 * 60 * 60));
        // 15s is plenty for an hour-long wait, and a tighter loop is rude to the
        // control plane for no benefit.
        assert_eq!(o.poll, Duration::from_secs(15));
        assert!(
            o.serial_bytes >= 4096,
            "a useful tail is more than a line or two"
        );
    }

    /// Two hours has to be expressible the way people write it.
    #[test]
    fn parsing_a_duration_flag() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("").is_err());
    }

    /// A finish that arrives on the same poll as the timeout is a finish. The guest
    /// does not owe us punctuality, and reporting a timeout on a completed install
    /// would send someone hunting a bug that is not there.
    #[test]
    fn finishing_beats_timing_out() {
        let mut w = watch();
        w.observe(S::Running, false, secs(20));
        w.observe(S::Running, true, secs(900));
        assert_eq!(
            w.observe(S::Stopped, false, TIMEOUT + secs(1)),
            Action::Finished
        );
    }
}
