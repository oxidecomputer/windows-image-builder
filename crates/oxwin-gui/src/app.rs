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

/// What the upload produced, or what it left behind when it did not.
#[derive(Debug, Clone, Default)]
pub struct UploadOutcome {
    pub disk: String,
    pub sent: u64,
    pub skipped: u64,
    /// Set only when the instance was created too.
    pub instance: Option<String>,
    pub system_disk: Option<String>,
    /// True things this app cannot fix — the tcp/3389 rule, chiefly.
    pub warnings: Vec<String>,
    /// Set only by a golden run. Its presence is what makes the summary render as a
    /// finished image rather than as an upload.
    pub image: Option<String>,
    /// What a golden run's teardown could not remove. Not a failure: the image
    /// exists, so saying nothing would leave a resource behind unmentioned.
    pub leftovers: Vec<String>,
}

pub struct Uploading {
    pub rx: Receiver<Event>,
    /// The structured result, separate from the event stream.
    ///
    /// `Event::Done` carries a `PathBuf` and a byte count, which is the shape of a
    /// finished *build*. An upload finishes with disk names, an optional instance and a
    /// list of warnings, and squeezing that through a path would mean parsing it back
    /// out on the other side.
    pub outcome: Receiver<Result<UploadOutcome, UploadFailure>>,
    pub cancel: Cancel,
    pub started: Instant,
    pub phase: String,
    pub detail: String,
    pub fraction: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct UploadFailure {
    pub message: String,
    /// Resources that exist on the rack because of the failed run, with the commands to
    /// remove them. Nothing is deleted automatically — see `oxwin_rack::instance`.
    pub leftovers: Vec<String>,
}

#[derive(Default)]
pub enum Upload {
    #[default]
    Idle,
    Running(Uploading),
    Done {
        outcome: UploadOutcome,
        elapsed: Duration,
    },
    Failed(UploadFailure),
}

impl Upload {
    pub fn is_running(&self) -> bool {
        matches!(self, Upload::Running(_))
    }
}

/// How far the Export stage should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UploadGoal {
    /// Create and upload the installer disk, and stop there.
    #[default]
    DiskOnly,
    /// Also create the system disk and the instance, booting from the installer.
    WholeInstance,
    /// The whole golden cycle: install, generalize, snapshot, image, tidy up.
    ///
    /// Only offered for an image this session built as a golden image. See
    /// `stages::golden_unavailable` for why that is keyed on the build rather than
    /// on the draft.
    GoldenImage,
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
    /// Names used for the upload, and for the copyable CLI commands beside it.
    pub project: String,
    pub disk_name: String,
    pub upload: Upload,
    pub upload_goal: UploadGoal,
    /// Logins found on this machine. Read once at startup; `oxide auth login` is not
    /// something this app does, so the list does not change while it runs.
    pub profiles: Vec<oxwin_rack::Profile>,
    /// Index into `profiles`.
    pub profile: usize,
    pub instance_name: String,
    pub system_disk_gib: String,
    /// Names every resource a golden run touches.
    pub run_name: String,
    pub keep: oxwin_rack::Keep,
    pub verify_clone: bool,
    /// Whether the image that was *built* is a golden image.
    ///
    /// Recorded when the build starts, not read from `draft` later: the draft stays
    /// editable afterwards, so reading it at render time would offer a golden run
    /// for an image that will never generalize itself. `None` means no build has
    /// happened in this session.
    pub built_golden: Option<bool>,
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
            upload: Upload::Idle,
            upload_goal: UploadGoal::default(),
            // Not being logged in is ordinary, so a failure to read the credentials is
            // an empty list here and a hint in the UI, not a startup error.
            profiles: oxwin_rack::profiles().unwrap_or_default(),
            profile: 0,
            instance_name: "windows".into(),
            run_name: "windows-golden".into(),
            keep: oxwin_rack::Keep::default(),
            verify_clone: false,
            built_golden: None,
            system_disk_gib: "100".into(),
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
        // Recorded here, from the settings this build is actually using. Reading the
        // draft again in stage 4 would be reading something the user may have edited
        // since, and the consequence is not cosmetic: a golden run on media that
        // does not generalize installs perfectly, never shuts down, and burns the
        // whole two-hour timeout before saying so.
        self.built_golden = Some(settings.deployment.is_golden());
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

    /// Run the whole golden cycle: install, generalize, snapshot, image, tidy up.
    ///
    /// The same shape as [`Self::start_upload`] — a thread, a channel of
    /// `progress::Event`, a `Cancel` — because `run_golden` emits exactly the same
    /// events. What differs is the wall clock: this takes about an hour, so the UI
    /// shows the command that continues it. Closing the window kills this thread and
    /// loses nothing on the rack.
    pub fn start_golden(&mut self) {
        let Some(artifact) = self.build.artifact().cloned() else {
            self.upload = Upload::Failed(UploadFailure {
                message: "there is no built image to work from".into(),
                leftovers: Vec::new(),
            });
            return;
        };
        let Some(profile) = self.profiles.get(self.profile).cloned() else {
            self.upload = Upload::Failed(UploadFailure {
                message: "no Oxide login found. Run `oxide auth login`".into(),
                leftovers: Vec::new(),
            });
            return;
        };
        let names = match oxwin_rack::Names::new(self.run_name.trim()) {
            Ok(names) => names,
            Err(e) => {
                self.upload = Upload::Failed(UploadFailure {
                    message: format!("{e:#}"),
                    leftovers: Vec::new(),
                });
                return;
            }
        };

        let cancel = Cancel::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        let reporter = Reporter::new(tx);

        let project = self.project.clone();
        let verify = self.verify_clone;
        let spec = oxwin_rack::golden::GoldenSpec {
            names: names.clone(),
            image_path: artifact,
            keep: self.keep,
            watch: oxwin_rack::golden::WatchOptions::default(),
            system_disk_gib: self
                .system_disk_gib
                .parse::<u64>()
                .unwrap_or(100)
                .max(1),
            ncpus: 4,
            memory_gib: 8,
            os: "windows".into(),
            // What the image reports about itself later. The media's own release,
            // detected during the build rather than asserted here.
            version: self
                .media
                .as_ref()
                .and_then(|m| m.release)
                .map(|r| r.label().to_string())
                .unwrap_or_else(|| "unknown".into()),
        };
        let cancel_for_thread = cancel.clone();

        self.upload = Upload::Running(Uploading {
            rx,
            outcome: outcome_rx,
            cancel,
            started: Instant::now(),
            phase: "starting".into(),
            detail: String::new(),
            fraction: None,
        });

        std::thread::spawn(move || {
            let result = (|| {
                let rack =
                    oxwin_rack::Rack::connect(&profile.selector, &project)
                        .map_err(|e| UploadFailure {
                            message: format!("{e:#}"),
                            leftovers: Vec::new(),
                        })?;
                let golden = rack
                    .run_golden(&spec, &reporter, &cancel_for_thread)
                    .map_err(|failure| UploadFailure {
                        message: format!("{:#}", failure.error),
                        // Nothing is torn down on failure, so name what exists.
                        leftovers: failure.leftovers.cleanup_commands(&project),
                    })?;

                // The clone check runs after the image, never instead of it: a clone
                // that fails to come up is a fact about the image, and the image is
                // still what the run produced.
                if verify
                    && let Err(e) = rack.verify_clone(
                        &spec.names,
                        &spec.watch,
                        &reporter,
                        &cancel_for_thread,
                    )
                {
                    return Err(UploadFailure {
                        message: format!(
                            "the image {} was made, but the clone check failed: \
                             {e:#}",
                            golden.image
                        ),
                        leftovers: Vec::new(),
                    });
                }

                Ok(UploadOutcome {
                    image: Some(golden.image),
                    leftovers: golden
                        .leftovers
                        .iter()
                        .map(|r| r.delete_command(&project))
                        .collect(),
                    ..Default::default()
                })
            })();
            let _ = outcome_tx.send(result);
        });
    }

    /// Upload the built image, and optionally create the instance too.
    ///
    /// Same shape as `start_build`: a thread, a channel of `progress::Event`, and a
    /// `Cancel`. The core's rule that it never prints and never blocks on a human is
    /// what makes `oxwin-rack` drop into this slot unchanged.
    pub fn start_upload(&mut self) {
        let Some(artifact) = self.build.artifact().cloned() else {
            self.upload = Upload::Failed(UploadFailure {
                message: "there is no built image to upload".into(),
                leftovers: Vec::new(),
            });
            return;
        };
        let Some(profile) = self.profiles.get(self.profile).cloned() else {
            self.upload = Upload::Failed(UploadFailure {
                message: "no Oxide login found. Run `oxide auth login`".into(),
                leftovers: Vec::new(),
            });
            return;
        };

        let cancel = Cancel::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        let reporter = Reporter::new(tx);

        let project = self.project.clone();
        let goal = self.upload_goal;
        let spec = oxwin_rack::DiskSpec {
            name: self.disk_name.clone(),
            description: "Windows installer, built by oxwin".into(),
            block_size: oxwin_rack::INSTALLER_BLOCK_SIZE,
        };
        let instance_name = self.instance_name.clone();
        let system_disk_gib =
            self.system_disk_gib.parse::<u64>().unwrap_or(100).max(1);
        let cancel_for_thread = cancel.clone();

        self.upload = Upload::Running(Uploading {
            rx,
            outcome: outcome_rx,
            cancel,
            started: Instant::now(),
            phase: "starting".into(),
            detail: String::new(),
            fraction: None,
        });

        std::thread::spawn(move || {
            let result = (|| {
                let rack =
                    oxwin_rack::Rack::connect(&profile.selector, &project)
                        .map_err(|e| UploadFailure {
                            message: format!("{e:#}"),
                            leftovers: Vec::new(),
                        })?;
                let uploaded = rack
                    .upload_image(
                        &artifact,
                        &spec,
                        &reporter,
                        &cancel_for_thread,
                    )
                    .map_err(|e| UploadFailure {
                        message: format!("{e:#}"),
                        // The disk is deliberately left in place; a failed upload of
                        // several gigabytes is worth keeping rather than discarding.
                        leftovers: vec![format!(
                            "oxide disk delete --project {project} --disk {}",
                            spec.name
                        )],
                    })?;

                let mut outcome = UploadOutcome {
                    disk: uploaded.disk.clone(),
                    sent: uploaded.sent,
                    skipped: uploaded.skipped,
                    ..Default::default()
                };
                if goal == UploadGoal::WholeInstance {
                    let mut instance = oxwin_rack::InstanceSpec::for_installer(
                        &instance_name,
                        &uploaded.disk,
                    );
                    instance.system_disk_gib = system_disk_gib;
                    match rack.create_instance(&instance, &reporter) {
                        Ok(created) => {
                            outcome.instance = Some(created.instance);
                            outcome.system_disk = created.system_disk;
                            outcome.warnings = created.warnings;
                        }
                        Err(failure) => {
                            let mut leftovers =
                                failure.leftovers.cleanup_commands(&project);
                            leftovers.push(format!(
                                "oxide disk delete --project {project} --disk {}",
                                uploaded.disk
                            ));
                            return Err(UploadFailure {
                                message: format!("{:#}", failure.error),
                                leftovers,
                            });
                        }
                    }
                }
                Ok(outcome)
            })();
            let _ = outcome_tx.send(result);
        });
    }

    /// Drain everything the upload thread has sent since the last frame.
    fn pump_upload(&mut self) {
        let mut finished: Option<Upload> = None;
        let mut logs: Vec<String> = Vec::new();

        if let Upload::Running(run) = &mut self.upload {
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
                    // The upload reports its result on its own channel, so these two
                    // are not expected here. Ignoring them keeps the loop total.
                    Ok(Event::Done { .. }) | Ok(Event::Failed { .. }) => {}
                    Err(_) => break,
                }
            }
            match run.outcome.try_recv() {
                Ok(Ok(outcome)) => {
                    finished = Some(Upload::Done {
                        outcome,
                        elapsed: run.started.elapsed(),
                    });
                }
                Ok(Err(failure)) => finished = Some(Upload::Failed(failure)),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = Some(Upload::Failed(UploadFailure {
                        message:
                            "the upload stopped without reporting a result"
                                .into(),
                        leftovers: Vec::new(),
                    }));
                }
            }
        }

        for l in logs {
            self.push_log(l);
        }
        if let Some(u) = finished {
            self.upload = u;
        }
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
        self.pump_upload();
        if self.build.is_running() || self.upload.is_running() {
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
