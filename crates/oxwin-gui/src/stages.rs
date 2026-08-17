// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The body of each stage. State lives in `app.rs`; this is presentation and the
//! small amount of glue that turns a click into a core call.

use crate::app::{App, Build, Stage};
use crate::theme;
use egui::{RichText, Ui};
use oxwin_core::WindowsRelease;
use std::path::{Path, PathBuf};

impl App {
    /// Drag-and-drop, handled at the top level so an ISO can be dropped anywhere
    /// in the window rather than only onto a target.
    pub fn accept_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect()
        });
        if dropped.is_empty() || self.build.is_running() {
            return;
        }
        if let Some(path) = dropped.into_iter().next() {
            self.set_iso(path);
            self.stage = Stage::Image;
        }
    }

    pub fn set_iso(&mut self, path: PathBuf) {
        self.draft.iso_note = describe_media(&path);
        self.draft.iso = Some(path);
        // A different source invalidates anything built from the old one.
        self.build = Build::Idle;
        self.saved_to = None;
    }

    // --- stage 1: image selection -----------------------------------------

    pub fn ui_image(&mut self, ui: &mut Ui) {
        let chosen = self.draft.iso.is_some();
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        // Optically centre the whole group. An empty stage with one job should not
        // leave its only control stranded near the top of the window.
        let box_size = egui::vec2(ui.available_width().min(560.0), 150.0);
        let button_block = if chosen { 68.0 } else { 0.0 };
        let error_block = if self.engine_error.is_some() { 96.0 } else { 0.0 };
        let content = box_size.y + button_block + error_block;
        ui.add_space(((ui.available_height() - content) * 0.42).max(0.0));

        let mut clicked = false;
        ui.vertical_centered(|ui| {
            let (rect, response) =
                ui.allocate_exact_size(box_size, egui::Sense::click());
            clicked = response.clicked();
            let active = hovering || response.hovered();
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            let painter = ui.painter();
            painter.rect_filled(
                rect,
                theme::CORNER,
                if active { theme::SECONDARY } else { theme::SURFACE },
            );
            // Dashed while the stage is unsatisfied, solid once it holds something.
            // The border state says whether this is still a target or now a result.
            if active {
                painter.rect_stroke(
                    rect,
                    theme::CORNER,
                    egui::Stroke::new(2.0, theme::PRIMARY),
                    egui::StrokeKind::Inside,
                );
            } else if chosen {
                painter.rect_stroke(
                    rect,
                    theme::CORNER,
                    egui::Stroke::new(1.0, theme::GREY),
                    egui::StrokeKind::Inside,
                );
            } else {
                dashed_rect(
                    painter,
                    rect,
                    egui::Stroke::new(1.5, theme::GREY),
                    theme::CORNER as f32,
                );
            }

            let (title, subtitle) = if hovering {
                ("Release to use this file".to_string(), String::new())
            } else {
                match &self.draft.iso {
                    Some(p) => (file_name(p), self.draft.iso_note.clone()),
                    None => (
                        "Drop a Windows ISO here".to_string(),
                        "or click to choose one".to_string(),
                    ),
                }
            };
            let title_rect = painter.text(
                rect.center() - egui::vec2(0.0, 11.0),
                egui::Align2::CENTER_CENTER,
                title,
                egui::FontId::proportional(16.0),
                theme::TEXT,
            );
            // The one accent on this stage: a filled dot meaning "this is ready".
            if chosen && !hovering {
                painter.circle_filled(
                    egui::pos2(title_rect.left() - 13.0, title_rect.center().y),
                    4.0,
                    theme::PRIMARY,
                );
            }
            if !subtitle.is_empty() {
                painter.text(
                    rect.center() + egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    subtitle,
                    egui::FontId::proportional(12.5),
                    theme::TEXT_DIM,
                );
            }
        });

        if clicked {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Windows installer", &["iso", "ISO"])
                .pick_file()
            {
                self.set_iso(p);
            }
        }

        if let Some(err) = &self.engine_error {
            ui.add_space(16.0);
            problem_box(
                ui,
                theme::DANGER,
                "This app cannot build images yet",
                err,
            );
        }

        // No disabled button while the stage is empty. Nothing to continue to yet,
        // and a greyed-out control is just an obstacle between the user and the
        // single thing this stage wants them to do.
        if chosen {
            ui.add_space(26.0);
            ui.vertical_centered(|ui| {
                if primary_button(ui, "Continue to Settings").clicked() {
                    self.stage = Stage::Settings;
                }
            });
        }
    }

    // --- stage 2: settings -------------------------------------------------

    pub fn ui_settings(&mut self, ui: &mut Ui) {
        heading(ui, "How should this Windows be set up?");

        // What stops a build, worked out before the action row is drawn.
        let blockers: Vec<String> = self
            .draft
            .to_settings()
            .problems()
            .into_iter()
            .filter(|p| p.blocking)
            .map(|p| p.message)
            .take(2)
            .collect();
        // The action row goes in a bottom panel, declared before the scroll area so
        // it claims its space first and the scroll area gets exactly what is left.
        // Hand-computing that reservation meant guessing at button heights and item
        // spacing, and the guess was wrong in one state or the other every time.
        egui::Panel::bottom("settings-actions").show(ui, |ui| {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                if blockers.is_empty() {
                    if primary_button(ui, "Build install media").clicked() {
                        self.start_chosen_build();
                    }
                } else {
                    ui.add_enabled(
                        false,
                        egui::Button::new("Build install media"),
                    );
                    ui.add_space(6.0);
                    // Say what is actually wrong, not that something is.
                    for message in &blockers {
                        ui.label(
                            RichText::new(message)
                                .color(theme::TEXT_DIM)
                                .size(12.0),
                        );
                    }
                }
            });
            ui.add_space(14.0);
        });
        egui::ScrollArea::vertical()
            // Fill the pane in both directions. Left to shrink, the scroll region
            // sizes itself to its content and floats inside the window instead of
            // occupying it.
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                // A picker with one option is not a choice, it is furniture. This
                // reappears on its own once another release has been verified.
                if WindowsRelease::ALL.len() > 1 {
                    section(ui, "Windows release");
                    ui.horizontal_wrapped(|ui| {
                        for r in WindowsRelease::ALL {
                            let mut label = RichText::new(r.label());
                            if !r.verified_on_hardware() {
                                label = label.color(theme::TEXT_DIM);
                            }
                            if ui
                                .selectable_label(self.draft.release == *r, label)
                                .clicked()
                            {
                                self.draft.release = *r;
                            }
                        }
                    });
                }

                section(ui, "Which install");
                for e in oxwin_core::Experience::ALL {
                    if ui.radio(self.draft.experience == *e, e.label()).clicked() {
                        self.draft.experience = *e;
                    }
                }
                hint(ui, self.draft.experience.description());

                section(ui, "What is this image for?");
                if ui
                    .radio(
                        self.draft.golden,
                        "A golden image — a template many machines clone",
                    )
                    .clicked()
                {
                    self.draft.golden = true;
                }
                if ui
                    .radio(
                        !self.draft.golden,
                        "One specific machine, with a fixed name",
                    )
                    .clicked()
                {
                    self.draft.golden = false;
                }
                if self.draft.golden {
                    hint(
                        ui,
                        "Each clone gets its own random computer name, so no two machines collide.",
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("Computer name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.hostname)
                                .desired_width(220.0),
                        );
                    });
                    hint(ui, "Up to 15 characters. Letters, digits and hyphens only.");
                }

                section(ui, "Administrator account");
                ui.horizontal(|ui| {
                    ui.label("Username");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.username).desired_width(200.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.password)
                            .password(!self.show_password)
                            .desired_width(200.0),
                    );
                    if ui.button("Generate").clicked() {
                        self.draft.password = oxwin_core::generate_password();
                        // Reveal it, or the user has no way to write down the thing
                        // they will need to log in with.
                        self.show_password = true;
                    }
                    ui.checkbox(&mut self.show_password, "Show");
                    if !self.draft.password.is_empty() && ui.button("Copy").clicked() {
                        ui.ctx().copy_text(self.draft.password.clone());
                        self.notice = Some("Password copied.".into());
                    }
                });
                hint(
                    ui,
                    "Windows needs a password: the serial console and Remote Desktop both \
                     sign in with one, and neither understands SSH keys.",
                );

                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "SSH public keys (optional) — for passwordless SSH, one per line",
                    )
                    .color(theme::TEXT_DIM)
                    .size(12.0),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.draft.keys_text)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("ssh-ed25519 AAAAC3Nza… you@laptop"),
                );
                if ui.button("Load from ~/.ssh…").clicked() {
                    let start = dirs_ssh();
                    let mut dlg = rfd::FileDialog::new().add_filter("public key", &["pub"]);
                    if let Some(d) = start {
                        dlg = dlg.set_directory(d);
                    }
                    if let Some(p) = dlg.pick_file() {
                        match std::fs::read_to_string(&p) {
                            Ok(text) => {
                                if !self.draft.keys_text.trim().is_empty()
                                    && !self.draft.keys_text.ends_with('\n')
                                {
                                    self.draft.keys_text.push('\n');
                                }
                                self.draft.keys_text.push_str(text.trim());
                                self.draft.keys_text.push('\n');
                            }
                            Err(e) => {
                                self.notice = Some(format!("Could not read {}: {e}", p.display()))
                            }
                        }
                    }
                }

                ui.add_space(6.0);
                section(ui, "Access and hardware");
                ui.checkbox(&mut self.draft.enable_ssh, "Install OpenSSH Server");
                ui.checkbox(&mut self.draft.enable_rdp, "Enable Remote Desktop");
                if self.draft.enable_rdp {
                    hint(
                        ui,
                        "RDP also needs a VPC firewall rule allowing tcp/3389 — the default VPC \
                         allows only SSH and ICMP. See the Guided Install stage.",
                    );
                }
                ui.checkbox(
                    &mut self.draft.enable_serial_console,
                    "Enable the serial console (recommended — it is how you watch the install)",
                );
                ui.checkbox(
                    &mut self.draft.inject_drivers,
                    "Inject virtio drivers (required for networking)",
                );

                ui.add_space(6.0);
                section(ui, "Optional");
                ui.horizontal(|ui| {
                    ui.label("Product key");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.product_key)
                            .desired_width(260.0)
                            .hint_text("leave empty for evaluation media"),
                    );
                });

                // Warnings live here, in the flow, because they are commentary on the
                // choices around them. Blockers do not: they belong beside the button
                // they are blocking, where they cannot be scrolled out of sight.
                let settings = self.draft.to_settings();
                for p in settings.problems().iter().filter(|p| !p.blocking) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("!").color(theme::WARNING));
                        ui.label(RichText::new(&p.message).color(theme::WARNING).size(12.5));
                    });
                }
                ui.add_space(12.0);
            });
    }

    /// Ask where the image should go, then start building it.
    fn start_chosen_build(&mut self) {
        let suggested = suggested_filename(&self.draft.to_settings());
        if let Some(dest) = rfd::FileDialog::new()
            .set_file_name(&suggested)
            .add_filter("disk image", &["img"])
            .save_file()
        {
            self.start_build(dest);
        }
    }

    // --- stage 3: processing ----------------------------------------------

    pub fn ui_processing(&mut self, ui: &mut Ui) {
        match &self.build {
            Build::Running(run) => {
                heading(ui, "Building your install media");
                let phase = run.phase.clone();
                let detail = run.detail.clone();
                let fraction = run.fraction;
                let elapsed = run.started.elapsed();

                hint(
                    ui,
                    &format!(
                        "{}  ·  {}",
                        phase_label(&phase),
                        fmt_duration(elapsed)
                    ),
                );
                ui.add_space(10.0);
                match fraction {
                    Some(f) => {
                        let mut bar = egui::ProgressBar::new(f)
                            .desired_height(14.0)
                            .fill(theme::PRIMARY);
                        if let Some(eta) = eta(elapsed, f) {
                            bar = bar.text(
                                RichText::new(format!(
                                    "{:.0}%  ·  about {} left",
                                    f * 100.0,
                                    eta
                                ))
                                .size(11.0),
                            );
                        }
                        ui.add(bar);
                    }
                    None => {
                        ui.add(
                            egui::ProgressBar::new(0.0)
                                .desired_height(14.0)
                                .fill(theme::PRIMARY)
                                .animate(true),
                        );
                    }
                }
                if !detail.is_empty() {
                    ui.label(
                        RichText::new(&detail)
                            .color(theme::TEXT_DIM)
                            .size(12.0),
                    );
                }

                ui.add_space(12.0);
                hint(
                    ui,
                    "Copying several gigabytes takes a few minutes. You can leave this running.",
                );

                ui.add_space(12.0);
                if ui.button("Cancel").clicked() {
                    if let Build::Running(run) = &self.build {
                        run.cancel.cancel();
                    }
                }
                self.log_pane(ui);
            }
            Build::Done { artifact, bytes, elapsed } => {
                let summary = format!(
                    "{} · {:.1} GiB · took {}",
                    file_name(artifact),
                    *bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    fmt_duration(*elapsed)
                );
                // Same action bar as every other stage, so the way forward is
                // always in the same place and always the same colour.
                egui::Panel::bottom("processing-actions").show(ui, |ui| {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        if primary_button(ui, "Continue to Export").clicked() {
                            self.stage = Stage::Export;
                        }
                    });
                    ui.add_space(14.0);
                });
                heading(ui, "Install media built");
                hint(ui, &summary);
                self.log_pane(ui);
            }
            Build::Failed { message } => {
                heading(ui, "The build did not finish");
                let message = message.clone();
                problem_box(ui, theme::DANGER, "Error", &message);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Back to Settings").clicked() {
                        self.stage = Stage::Settings;
                    }
                    if let Some(out) = self.out_path.clone() {
                        if ui.button("Try again").clicked() {
                            self.start_build(out);
                        }
                    }
                });
                self.log_pane(ui);
            }
            Build::Idle => {
                heading(ui, "Ready to build");
                hint(
                    ui,
                    "Choose where to write the image and the build will start.",
                );
                ui.add_space(14.0);
                self.footer_build(ui, self.draft.to_settings().is_buildable());
            }
        }
    }

    // --- stage 4: export ---------------------------------------------------

    pub fn ui_export(&mut self, ui: &mut Ui) {
        let Some(artifact) = self.build.artifact().cloned() else {
            heading(ui, "Nothing to export yet");
            hint(ui, "Build an image first.");
            return;
        };

        heading(ui, "Export your install media");

        // Same shape as the Settings stage: action row in a bottom panel first, then
        // a scroll area that fills whatever is left.
        egui::Panel::bottom("export-actions").show(ui, |ui| {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                if primary_button(ui, "Continue to Guided Install").clicked() {
                    self.stage = Stage::Install;
                }
                if let Some(n) = self.notice.clone() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(n).color(theme::PRIMARY).size(12.0));
                }
            });
            ui.add_space(14.0);
        });

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                hint(
                    ui,
                    "Two ways to get this onto a rack. They are alternatives — pick whichever \
                     suits your environment.",
                );

                section(ui, "Save the image file");
                hint(
                    ui,
                    "For airgapped racks, or when someone else does the upload.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Save a copy…").clicked() {
                        let name = file_name(&artifact);
                        if let Some(dest) = rfd::FileDialog::new().set_file_name(&name).save_file()
                        {
                            match std::fs::copy(&artifact, &dest) {
                                Ok(_) => {
                                    self.saved_to = Some(dest.clone());
                                    self.notice = Some(format!("Saved to {}", dest.display()));
                                }
                                Err(e) => self.notice = Some(format!("Could not save: {e}")),
                            }
                        }
                    }
                    ui.label(
                        RichText::new(format!("currently at {}", artifact.display()))
                            .color(theme::TEXT_DIM)
                            .size(11.5),
                    );
                });
                if let Some(saved) = &self.saved_to {
                    ui.label(
                        RichText::new(format!("Saved to {}", saved.display()))
                            .color(theme::PRIMARY)
                            .size(12.0),
                    );
                }

                section(ui, "Upload to a rack");
                hint(
                    ui,
                    "In-app upload is not wired up yet. These are the commands that do it — \
                     they use the login you already have from `oxide auth login`.",
                );
                hint(
                    ui,
                    "The 512-byte block size is required, not a preference: this image's \
                     partition table is laid out in 512-byte sectors, and a disk with \
                     larger blocks will not boot.",
                );
                ui.horizontal(|ui| {
                    ui.label("Project");
                    ui.add(egui::TextEdit::singleline(&mut self.project).desired_width(180.0));
                    ui.add_space(12.0);
                    ui.label("Disk name");
                    ui.add(egui::TextEdit::singleline(&mut self.disk_name).desired_width(180.0));
                });
                let cmd = upload_command(&artifact, &self.project, &self.disk_name);
                code_block(ui, &cmd);
                if ui.button("Copy command").clicked() {
                    ui.ctx().copy_text(cmd);
                    self.notice = Some("Command copied.".into());
                }
                ui.add_space(12.0);
            });
    }

    // --- stage 5: guided install ------------------------------------------

    pub fn ui_install(&mut self, ui: &mut Ui) {
        heading(ui, "Installing on the rack");
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let settings = self.draft.to_settings();

                step(ui, 1, "Upload the image as a disk");
                hint(
                    ui,
                    "Several gigabytes over the network — expect this to be the slow part.",
                );
                code_block(
                    ui,
                    &upload_command(
                        self.build
                            .artifact()
                            .map(|p| p.as_path())
                            .unwrap_or(Path::new("windows-install.img")),
                        &self.project,
                        &self.disk_name,
                    ),
                );

                step(ui, 2, "Create the disk Windows will install onto");
                hint(
                    ui,
                    "This one starts blank. 100 GiB is a reasonable starting point.",
                );
                code_block(
                    ui,
                    &format!(
                        "oxide disk create --project {} --name windows-system \\\n  \
                     --description 'Windows system disk' --size 100GiB",
                        self.project
                    ),
                );

                step(ui, 3, "Create the instance, booting from the installer");
                hint(
                ui,
                "The installer disk must be the boot disk for the first boot, and the instance \
                 needs an external IP if you want to reach it.",
            );
                code_block(ui, &instance_json(&self.project, &self.disk_name));
                code_block(
                    ui,
                    &format!(
                        "oxide instance create --project {} --json-body instance.json",
                        self.project
                    ),
                );

                step(ui, 4, "Watch it install");
                hint(
                    ui,
                    "Setup reboots a few times. That is normal — let it finish.",
                );
                code_block(
                    ui,
                    &format!(
                        "oxide instance serial console --project {} --instance windows",
                        self.project
                    ),
                );

                step(ui, 5, "Log in");
                let username = &settings.credentials.username;
                if settings.credentials.keys.is_empty() {
                    hint(
                        ui,
                        "With the username and password you set on the Settings stage.",
                    );
                } else {
                    hint(
                        ui,
                        "Your SSH key works for SSH; the password is what the serial console \
                     and Remote Desktop ask for.",
                    );
                }
                code_block(ui, &format!("ssh {username}@<instance-ip>"));
                if settings.enable_rdp {
                    ui.add_space(4.0);
                    problem_box(
                    ui,
                    theme::WARNING,
                    "Remote Desktop needs a firewall rule",
                    "Enabling RDP inside Windows is not enough. The default VPC allows only SSH \
                     and ICMP, so tcp/3389 is dropped before it reaches the guest. Add a rule \
                     allowing 3389 inbound, or RDP will simply time out.",
                );
                }

                step(ui, 6, "Remove the installer disk when you are done");
                hint(
                ui,
                "The installer is safe to leave attached — it detects an installed Windows and \
                 boots that instead. But once the install is finished you do not need it, and \
                 detaching it removes any chance of a surprise later.",
            );
                code_block(
                    ui,
                    &format!(
                        "oxide instance disk detach --project {} --instance windows --disk {}",
                        self.project, self.disk_name
                    ),
                );
                ui.add_space(20.0);
            });
    }

    // --- shared pieces -----------------------------------------------------

    fn log_pane(&mut self, ui: &mut Ui) {
        ui.add_space(14.0);
        let label = if self.show_log { "Hide details" } else { "Show details" };
        if ui
            .add(
                egui::Button::new(RichText::new(label).size(12.0)).frame(false),
            )
            .clicked()
        {
            self.show_log = !self.show_log;
        }
        if !self.show_log {
            return;
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .max_height(190.0)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(
                        RichText::new(line)
                            .monospace()
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                }
            });
    }

    /// The footer that actually starts a build, since that needs a destination.
    fn footer_build(&mut self, ui: &mut Ui, ready: bool) {
        ui.vertical_centered(|ui| {
            let clicked = if ready {
                primary_button(ui, "Build install media").clicked()
            } else {
                ui.add_enabled(false, egui::Button::new("Build install media"))
                    .clicked()
            };
            if clicked {
                let suggested = suggested_filename(&self.draft.to_settings());
                if let Some(dest) = rfd::FileDialog::new()
                    .set_file_name(&suggested)
                    .add_filter("disk image", &["img"])
                    .save_file()
                {
                    self.start_build(dest);
                }
            }
            if !ready {
                ui.label(
                    RichText::new("Resolve the items above first")
                        .color(theme::TEXT_DIM)
                        .size(12.0),
                );
            }
        });
    }
}

// --- small widgets --------------------------------------------------------

/// The one action a stage most wants taken. Filled in the primary colour with dark
/// text, so it reads as the destination rather than as another grey control.
fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(13.5)
                .strong()
                .color(egui::Color32::from_rgb(0x04, 0x1a, 0x14)),
        )
        .fill(theme::PRIMARY)
        .corner_radius(theme::CORNER)
        .min_size(egui::vec2(200.0, 34.0)),
    )
}

/// egui has no dashed rectangle, so stitch one from four dashed edges.
///
/// The edges stop short by the corner radius rather than meeting at square corners,
/// which would fight the rounded fill underneath. Open corners read as deliberate;
/// a square dashed outline around a rounded panel just reads as a mistake.
fn dashed_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    stroke: egui::Stroke,
    radius: f32,
) {
    let r = rect.shrink(stroke.width * 0.5);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let edges = [
        [egui::pos2(l + radius, t), egui::pos2(rt - radius, t)],
        [egui::pos2(rt, t + radius), egui::pos2(rt, b - radius)],
        [egui::pos2(rt - radius, b), egui::pos2(l + radius, b)],
        [egui::pos2(l, b - radius), egui::pos2(l, t + radius)],
    ];
    for edge in edges {
        painter.extend(egui::Shape::dashed_line(&edge, stroke, 7.0, 5.0));
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(19.0).strong().color(theme::TEXT));
    ui.add_space(2.0);
}

/// A group label. Deliberately not the primary colour: green carries meaning in this
/// app — a stage is done, a control is on, this is the action to take. Spending it on
/// four static headings per screen drains it of all of that.
fn section(ui: &mut Ui, text: &str) {
    ui.add_space(18.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.5)
            .strong()
            .color(theme::TEXT_DIM),
    );
    ui.add_space(5.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::TEXT_DIM));
}

fn step(ui: &mut Ui, n: usize, title: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui
            .allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 11.0, theme::SECONDARY);
        ui.painter().circle_stroke(
            rect.center(),
            11.0,
            egui::Stroke::new(1.5, theme::PRIMARY),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{n}"),
            egui::FontId::proportional(11.5),
            theme::PRIMARY,
        );
        ui.label(RichText::new(title).size(14.5).strong().color(theme::TEXT));
    });
}

fn code_block(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::GREY))
        .inner_margin(egui::Margin::same(10))
        .corner_radius(theme::CORNER)
        .show(ui, |ui| {
            // Fill the column rather than sitting at a fixed width that stops short
            // of the window edge.
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new(text).monospace().size(11.5).color(theme::TEXT),
            );
        });
}

fn problem_box(ui: &mut Ui, colour: egui::Color32, title: &str, body: &str) {
    ui.add_space(8.0);
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, colour))
        .inner_margin(egui::Margin::same(10))
        .corner_radius(theme::CORNER)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).size(12.5).strong().color(colour));
            ui.label(RichText::new(body).size(12.0).color(theme::TEXT_DIM));
        });
}

// --- helpers --------------------------------------------------------------

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn dirs_ssh() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh"))
}

/// A quick, non-blocking sanity note about the chosen source. Mounting the ISO to
/// check properly happens at build time; doing it here would freeze the UI.
pub(crate) fn describe_media(path: &Path) -> String {
    if path.is_dir() {
        let wim = path.join("sources/install.wim");
        let esd = path.join("sources/install.esd");
        for candidate in [&wim, &esd] {
            if let Ok(m) = std::fs::metadata(candidate) {
                return format!(
                    "mounted installer · {} is {:.2} GiB",
                    file_name(candidate),
                    m.len() as f64 / (1024.0 * 1024.0 * 1024.0)
                );
            }
        }
        return "this folder has no sources/install.wim — is it really Windows media?".into();
    }
    match std::fs::metadata(path) {
        Ok(m) => {
            let gib = m.len() as f64 / (1024.0 * 1024.0 * 1024.0);
            if gib < 1.0 {
                format!("{gib:.2} GiB — that looks too small for a Windows ISO")
            } else {
                format!("{gib:.2} GiB")
            }
        }
        Err(e) => format!("cannot read this file: {e}"),
    }
}

fn suggested_filename(s: &Settingsish) -> String {
    let name = match &s.deployment {
        oxwin_core::Deployment::GoldenImage => "golden".to_string(),
        oxwin_core::Deployment::Named { hostname } => {
            let h = hostname.trim();
            if h.is_empty() { "windows".into() } else { h.to_ascii_lowercase() }
        }
    };
    format!("{}-{}-install.img", s.release.token(), name)
}

type Settingsish = oxwin_core::Settings;

/// The block size the image is built for. Not a preference — the MBR expresses
/// partition offsets in sectors of this size, so p1 at LBA 2048 means 1 MiB. Import
/// the same bytes onto a 4096-byte-block disk and LBA 2048 means 8 MiB instead: the
/// partition table points at nothing and the disk does not boot. Passed explicitly
/// rather than relying on whatever the CLI defaults to.
const DISK_BLOCK_SIZE: u32 = 512;

fn upload_command(image: &Path, project: &str, disk: &str) -> String {
    format!(
        "oxide disk import \\\n  --project {project} \\\n  --disk {disk} \\\n  \
         --disk-block-size {DISK_BLOCK_SIZE} \\\n  \
         --description 'Windows install media' \\\n  --path {}",
        image.display()
    )
}

fn instance_json(project: &str, installer_disk: &str) -> String {
    // Written out rather than built with a JSON library: the point is for someone
    // to read it, understand which disk boots, and edit it.
    format!(
        "# instance.json  (project: {project})\n\
         {{\n  \
           \"name\": \"windows\",\n  \
           \"description\": \"Windows server\",\n  \
           \"hostname\": \"windows\",\n  \
           \"ncpus\": 4,\n  \
           \"memory\": 8589934592,\n  \
           \"boot_disk\": {{ \"type\": \"attach\", \"name\": \"{installer_disk}\" }},\n  \
           \"disks\": [\n    \
             {{ \"type\": \"attach\", \"name\": \"windows-system\" }}\n  \
           ],\n  \
           \"external_ips\": [ {{ \"type\": \"ephemeral\" }} ],\n  \
           \"start\": true\n\
         }}"
    )
}

fn phase_label(phase: &str) -> &str {
    match phase {
        "layout" => "Laying out the image",
        "media" => "Adding setup files, drivers and OpenSSH",
        "boot" => "Writing the boot loader",
        "copy" => "Copying the Windows image",
        other => other,
    }
}

fn fmt_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// Remaining time from elapsed time and progress. Meaningless below a few percent,
/// so it is not shown until the estimate has something to stand on.
fn eta(elapsed: std::time::Duration, fraction: f32) -> Option<String> {
    if fraction < 0.03 {
        return None;
    }
    let total = elapsed.as_secs_f32() / fraction;
    let left = total - elapsed.as_secs_f32();
    if !left.is_finite() || left < 0.0 {
        return None;
    }
    Some(fmt_duration(std::time::Duration::from_secs_f32(left)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxwin_core::{Deployment, Settings};

    #[test]
    fn suggested_names_are_filesystem_safe_and_descriptive() {
        let s = Settings {
            deployment: Deployment::Named { hostname: "WIN-A".into() },
            ..Default::default()
        };
        assert_eq!(suggested_filename(&s), "ws2022-win-a-install.img");
        let g = Settings {
            deployment: Deployment::GoldenImage,
            ..Default::default()
        };
        assert_eq!(suggested_filename(&g), "ws2022-golden-install.img");
    }

    #[test]
    fn eta_waits_for_a_meaningful_sample() {
        assert!(eta(std::time::Duration::from_secs(5), 0.001).is_none());
        assert!(eta(std::time::Duration::from_secs(60), 0.5).is_some());
    }

    #[test]
    fn upload_command_names_the_real_flags() {
        let c = upload_command(Path::new("/x/y.img"), "danb", "wininst");
        assert!(c.contains("oxide disk import"));
        assert!(c.contains("--project danb"));
        assert!(c.contains("--disk wininst"));
        assert!(c.contains("--path /x/y.img"));
        // The image's partition offsets are in 512-byte sectors. A 4096-byte-block
        // disk shifts every one of them and nothing boots, so this must be explicit
        // rather than left to the CLI's default.
        assert!(c.contains("--disk-block-size 512"));
    }
}
