// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Build progress, as data.
//!
//! The engine never prints and never blocks on a human, it emits these. That is
//! what lets the same engine drive a GUI progress bar, a CLI spinner, and a test
//! that just collects the events and asserts on them. Really want the system to
//! not feel frozen when doing longer running events.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Entered a named phase of the build. Coarse, discrete, ordered.
    Phase {
        name: String,
        message: String,
    },
    /// Fine-grained progress within the dominant phase. `fraction` is 0.0..=1.0.
    Fraction {
        fraction: f32,
        detail: String,
    },
    /// Human-readable output from the engine, for the log pane.
    Log(String),
    Done {
        artifact: PathBuf,
        bytes: u64,
    },
    Failed {
        message: String,
    },
}

/// Where events go. Cloneable and cheap, so the engine can hand it to threads.
#[derive(Clone)]
pub struct Reporter(Option<Sender<Event>>);

impl Reporter {
    pub fn new(tx: Sender<Event>) -> Self {
        Self(Some(tx))
    }

    /// A reporter that discards everything, for tests and for the CLI's quiet mode.
    pub fn silent() -> Self {
        Self(None)
    }

    /// Send an event. A closed channel is not an error: the UI may have gone away
    /// while a long build is still running, and that must not fail the build.
    pub fn send(&self, event: Event) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(event);
        }
    }

    pub fn phase(&self, name: impl Into<String>, message: impl Into<String>) {
        self.send(Event::Phase { name: name.into(), message: message.into() });
    }

    pub fn log(&self, line: impl Into<String>) {
        self.send(Event::Log(line.into()));
    }

    pub fn fraction(&self, fraction: f32, detail: impl Into<String>) {
        self.send(Event::Fraction {
            fraction: fraction.clamp(0.0, 1.0),
            detail: detail.into(),
        });
    }
}
