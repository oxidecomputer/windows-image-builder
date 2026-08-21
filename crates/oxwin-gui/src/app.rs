// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Application state and the frame loop.
//!
//! The form fields live in [`Draft`] as plain strings and bools, and are converted
//! into a core [`Settings`] on demand. Editing the core enums in place through
//! widgets would mean losing a half-typed hostname every time a radio button
//! changed, so the GUI keeps its own raw state and the core keeps the meaning.

use crate::stepper::{self, State};
use crate::theme;
use oxwin_core::media::MediaInfo;
use oxwin_core::{
    Cancel, Credentials, Deployment, Engine, Event, Reporter, Settings,
    WindowsRelease,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub const STAGES: &[&str] =
    &["Image Selection", "Settings", "Processing", "Export", "Guided Install"];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Image = 0,
    Settings = 1,
    Processing = 2,
    Export = 3,
    Install = 4,
}

impl Stage {
    pub(crate) fn from_index(i: usize) -> Stage {
        match i {
            0 => Stage::Image,
            1 => Stage::Settings,
            2 => Stage::Processing,
            3 => Stage::Export,
            _ => Stage::Install,
        }
    }
}

pub struct Draft {
    pub iso: Option<PathBuf>,
    pub iso_note: String,
    /// Detected from the media, not chosen. Displayed, never edited.
    pub release: WindowsRelease,
    /// Index of the image picked out of the media's own list. `None` until the media has
    /// been read, at which point stage 2 selects a sensible default.
    pub image_index: Option<u32>,
    pub golden: bool,
    pub hostname: String,
    pub username: String,
    pub password: String,
    pub keys_text: String,
    pub enable_ssh: bool,
    pub enable_rdp: bool,
    pub inject_drivers: bool,
    pub enable_serial_console: bool,
    pub product_key: String,
    pub target_disk: u8,
}

impl Default for Draft {
    fn default() -> Self {
        let d = Settings::default();
        Self {
            iso: None,
            iso_note: String::new(),
            release: d.release,
            image_index: None,
            golden: true,
            // A real value, not a placeholder. Choosing "one specific machine" should
            // leave you with something that already works and can be typed over.
            hostname: "win-server-01".into(),
            username: "oxide".into(),
            password: String::new(),
            keys_text: String::new(),
            enable_ssh: d.enable_ssh,
            enable_rdp: d.enable_rdp,
            inject_drivers: d.inject_drivers,
            enable_serial_console: d.enable_serial_console,
            product_key: String::new(),
            target_disk: d.target_disk,
        }
    }
}

impl Draft {
    pub fn keys(&self) -> Vec<String> {
        self.keys_text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            iso: self.iso.clone().unwrap_or_default(),
            release: self.release,
            // The index is the only unambiguous selector: on server media two images
            // share `ServerDatacenterEval`, differing only in Core-ness. Before the media
            // has been read there is no index, and the default hint stands in.
            edition: match self.image_index {
                Some(index) => index.to_string(),
                None => Settings::default().edition,
            },
            deployment: if self.golden {
                Deployment::GoldenImage
            } else {
                Deployment::Named { hostname: self.hostname.trim().to_string() }
            },
            credentials: Credentials {
                username: self.username.trim().to_string(),
                password: self.password.clone(),
                keys: self.keys(),
            },
            enable_ssh: self.enable_ssh,
            enable_rdp: self.enable_rdp,
            inject_drivers: self.inject_drivers,
            enable_serial_console: self.enable_serial_console,
            product_key: {
                let k = self.product_key.trim();
                if k.is_empty() { None } else { Some(k.to_string()) }
            },
            target_disk: self.target_disk,
        }
    }
}

pub struct Running {
    pub rx: Receiver<Event>,
    pub cancel: Cancel,
    pub started: Instant,
    pub phase: String,
    pub detail: String,
    pub fraction: Option<f32>,
}

pub enum Build {
    Idle,
    Running(Running),
    Done { artifact: PathBuf, bytes: u64, elapsed: Duration },
    Failed { message: String },
}

impl Build {
    pub fn is_running(&self) -> bool {
        matches!(self, Build::Running(_))
    }
    pub fn artifact(&self) -> Option<&PathBuf> {
        match self {
            Build::Done { artifact, .. } => Some(artifact),
            _ => None,
        }
    }
}

pub struct App {
    pub stage: Stage,
    pub draft: Draft,
    pub build: Build,
    pub engine: Option<Arc<Engine>>,
    pub engine_error: Option<String>,
    /// What the chosen media says it is. Read once, when the file is picked — about ten
    /// milliseconds, so there is nothing to background — and then every stage can offer
    /// real choices instead of asking the user to assert them.
    pub media: Option<MediaInfo>,
    /// Why the media could not be read at all. Distinct from `media.problems()`, which
    /// is media that was read and is unusable.
    pub media_error: Option<String>,
    /// Where the build was told to write. Kept so a cancelled or failed run can be
    /// cleaned up and a retry can reuse the same destination.
    pub out_path: Option<PathBuf>,
    pub saved_to: Option<PathBuf>,
    pub log: Vec<String>,
    pub show_log: bool,
    /// Whether the password field is revealed. Off by default, on right after
    /// generating one, since a password you cannot read is useless.
    pub show_password: bool,
    pub notice: Option<String>,
    /// Names used to generate the copyable CLI commands on the Export stage.
    pub project: String,
    pub disk_name: String,
}

const LOG_CAP: usize = 2000;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        // Reported on the first screen rather than several minutes into a build. The
        // engine itself always constructs; what can be missing is its payload.
        let engine = Engine::discover();
        let engine_error = engine.assets().problem();
        let engine = Some(Arc::new(engine));
        let draft = Draft::default();
        // Opening the app on a file, the way any desktop app should behave.
        let opened_with =
            std::env::args().nth(1).map(PathBuf::from).filter(|p| p.exists());

        // Debug builds only: jump straight to a stage so its layout can be reviewed
        // without clicking through. Compiled out of release entirely.
        // `mut` only matters in a debug build, where OXWIN_START_STAGE can move it.
        #[cfg_attr(not(debug_assertions), allow(unused_mut))]
        let mut stage = Stage::Image;
        #[cfg(debug_assertions)]
        if let Ok(n) = std::env::var("OXWIN_START_STAGE") {
            if let Ok(i) = n.parse::<usize>() {
                stage = Stage::from_index(i);
            }
        }

        let mut app = Self {
            stage,
            draft,
            build: Build::Idle,
            engine,
            engine_error,
            media: None,
            media_error: None,
            out_path: None,
            saved_to: None,
            log: Vec::new(),
            show_log: false,
            show_password: false,
            notice: None,
            project: "my-project".into(),
            disk_name: "windows-install".into(),
        };
        // Through `set_iso` rather than by assigning the field, so a file passed on the
        // command line is inspected exactly like one that was dropped.
        if let Some(path) = opened_with {
            app.set_iso(path);
        }
        app
    }

    /// Whether the chosen media can be built at all. Arm64 media reaches here looking
    /// perfectly normal, so this is what stops the user carrying it to stage 2 and only
    /// finding out when the build refuses.
    pub fn media_ok(&self) -> bool {
        self.media_error.is_none()
            && self.media.as_ref().is_none_or(|m| m.is_buildable())
    }

    pub fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_CAP {
            let excess = self.log.len() - LOG_CAP;
            self.log.drain(0..excess);
        }
    }

    /// Whether a stage counts as finished, which drives the stepper's colour.
    pub fn state_of(&self, i: usize) -> State {
        let stage = Stage::from_index(i);
        if stage == self.stage {
            return State::Current;
        }
        let done = match stage {
            Stage::Image => self.draft.iso.is_some() && self.media_ok(),
            Stage::Settings => {
                self.draft.iso.is_some()
                    && self.media_ok()
                    && self.draft.to_settings().is_buildable()
            }
            Stage::Processing => self.build.artifact().is_some(),
            Stage::Export => self.saved_to.is_some(),
            Stage::Install => false,
        };
        if done { State::Done } else { State::Todo }
    }

    /// Stages the user may jump back to. Never allow navigation away from a build
    /// in flight to a stage that would let settings change underneath it.
    pub fn reachable(&self, i: usize) -> bool {
        let stage = Stage::from_index(i);
        if self.build.is_running() {
            return stage == Stage::Processing;
        }
        match stage {
            Stage::Image => true,
            Stage::Settings => self.draft.iso.is_some() && self.media_ok(),
            Stage::Processing => {
                self.draft.iso.is_some()
                    && self.media_ok()
                    && self.draft.to_settings().is_buildable()
            }
            Stage::Export | Stage::Install => self.build.artifact().is_some(),
        }
    }

    pub fn start_build(&mut self, out: PathBuf) {
        let Some(engine) = self.engine.clone() else {
            self.build = Build::Failed {
                message: self
                    .engine_error
                    .clone()
                    .unwrap_or_else(|| "no build engine available".into()),
            };
            return;
        };
        let settings = self.draft.to_settings();
        let cancel = Cancel::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let reporter = Reporter::new(tx);
        let out_for_thread = out.clone();
        let cancel_for_thread = cancel.clone();

        self.log.clear();
        self.out_path = Some(out);
        self.saved_to = None;
        self.notice = None;
        self.build = Build::Running(Running {
            rx,
            cancel,
            started: Instant::now(),
            phase: "starting".into(),
            detail: String::new(),
            fraction: None,
        });
        self.stage = Stage::Processing;

        std::thread::spawn(move || {
            let result = engine.build(
                &settings,
                &out_for_thread,
                &reporter,
                &cancel_for_thread,
            );
            match result {
                Ok(artifact) => {
                    let bytes = std::fs::metadata(&artifact)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    reporter.send(Event::Done { artifact, bytes });
                }
                Err(e) => {
                    reporter.send(Event::Failed { message: format!("{e:#}") })
                }
            }
        });
    }

    /// Drain everything the build thread has sent since the last frame.
    fn pump(&mut self) {
        let mut finished: Option<Build> = None;
        let mut logs: Vec<String> = Vec::new();

        if let Build::Running(run) = &mut self.build {
            loop {
                match run.rx.try_recv() {
                    Ok(Event::Phase { name, message }) => {
                        run.phase = name;
                        run.detail = message.clone();
                        logs.push(message);
                    }
                    Ok(Event::Fraction { fraction, detail }) => {
                        run.fraction = Some(fraction);
                        run.detail = detail;
                    }
                    Ok(Event::Log(line)) => logs.push(line),
                    Ok(Event::Done { artifact, bytes }) => {
                        finished = Some(Build::Done {
                            artifact,
                            bytes,
                            elapsed: run.started.elapsed(),
                        });
                        break;
                    }
                    Ok(Event::Failed { message }) => {
                        finished = Some(Build::Failed { message });
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // The thread always sends a terminal event before exiting,
                        // so reaching here means it died unexpectedly.
                        if finished.is_none() {
                            finished = Some(Build::Failed {
                                message: "the build stopped without reporting a result".into(),
                            });
                        }
                        break;
                    }
                }
            }
        }

        for l in logs {
            self.push_log(l);
        }
        if let Some(b) = finished {
            let advance = matches!(b, Build::Done { .. });
            self.build = b;
            if advance {
                self.stage = Stage::Export;
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Cheap to clone (it is a handle), and cloning frees `ui` for &mut use below.
        let ctx = ui.ctx().clone();
        self.pump();
        if self.build.is_running() {
            // Progress arrives from another thread, which does not wake the UI.
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        self.accept_dropped_files(&ctx);

        // The stepper is the only thing up here. The window title already says what
        // the app is, and which engine built an image belongs in the log, not in
        // permanent chrome.
        egui::Panel::top("stepper").exact_size(94.0).show(ui, |ui| {
            ui.add_space(12.0);
            let clicked = stepper::show(
                ui,
                STAGES,
                |i| self.state_of(i),
                |i| self.reachable(i) && Stage::from_index(i) != self.stage,
            );
            if let Some(i) = clicked {
                self.stage = Stage::from_index(i);
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            // One margin for every stage, so no stage body has to remember to keep
            // itself off the window edge.
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 30,
                    right: 30,
                    top: 10,
                    bottom: 8,
                })
                .show(ui, |ui| match self.stage {
                    Stage::Image => self.ui_image(ui),
                    Stage::Settings => self.ui_settings(ui),
                    Stage::Processing => self.ui_processing(ui),
                    Stage::Export => self.ui_export(ui),
                    Stage::Install => self.ui_install(ui),
                });
        });
    }
}
