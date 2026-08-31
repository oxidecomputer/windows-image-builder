// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The body of each stage. State lives in `app.rs`; this is presentation and the
//! small amount of glue that turns a click into a core call.

use crate::app::{App, Build, Stage};
use crate::theme;
use egui::{RichText, Ui};
use oxwin_core::media::{self, Media, MediaInfo};
use oxwin_core::wim;
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
        self.draft.iso = Some(path.clone());
        // A different source invalidates anything built from the old one.
        self.build = Build::Idle;
        self.saved_to = None;

        // Read the media now rather than at build time. It costs about ten milliseconds
        // — a UDF walk and two seeks — and it is what lets stage 2 offer the editions
        // this ISO actually carries instead of a list someone typed into a table.
        self.media = None;
        self.media_error = None;
        self.draft.image_index = None;
        match media::inspect(&Media::at(path)) {
            Ok(info) => {
                if let Some(release) = info.release {
                    self.draft.release = release;
                }
                self.draft.image_index =
                    media::default_image(&info.images).map(|i| i.index);
                self.media = Some(info);
            }
            Err(e) => self.media_error = Some(format!("{e:#}")),
        }
    }

    // --- stage 1: image selection -----------------------------------------

    pub fn ui_image(&mut self, ui: &mut Ui) {
        let chosen = self.draft.iso.is_some();
        let ready = chosen && self.media_ok();
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
            // Green means state, so it must not appear beside media that was read and
            // refused — the file is present, which is not the same as usable.
            if chosen && !hovering && ready {
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

        // What the ISO turned out to be. Shown here, on the stage where it was chosen,
        // so picking the wrong file is caught immediately rather than at stage 2 — and
        // so media this app refuses says so before anything else is filled in.
        if let Some(err) = &self.media_error {
            ui.add_space(16.0);
            problem_box(
                ui,
                theme::DANGER,
                "This does not look like Windows installation media",
                err,
            );
        } else if let Some(info) = &self.media {
            ui.add_space(16.0);
            match info.problems().into_iter().find(|p| p.blocking) {
                Some(blocking) => problem_box(
                    ui,
                    theme::DANGER,
                    "This media cannot be used",
                    &blocking.message,
                ),
                None => {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(describe_detection(info))
                                .color(theme::PRIMARY)
                                .size(13.0),
                        );
                    });
                }
            }
        }

        // No disabled button while the stage is empty. Nothing to continue to yet,
        // and a greyed-out control is just an obstacle between the user and the
        // single thing this stage wants them to do.
        if chosen && self.media_ok() {
            ui.add_space(26.0);
            ui.vertical_centered(|ui| {
                if primary_button(ui, "Continue to Settings").clicked() {
                    self.stage = Stage::Settings;
                }
            });
        }
    }

    /// What this media is, and which of its images to install.
    ///
    /// Two radio groups used to stand here: the Windows release, and Desktop against
    /// Core. Both were assertions nothing checked. The release is now read off the ISO
    /// and only displayed, and Core-ness is a property of the row picked rather than a
    /// switch beside it — which also means client media, where nothing is Core and there
    /// are eleven images rather than four, needs no special case.
    fn ui_edition_picker(&mut self, ui: &mut Ui) {
        let Some(info) = &self.media else {
            // Reachable only if the media could not be read at all; stage 1 does not let
            // an unreadable ISO through, so this is a belt-and-braces case.
            if let Some(err) = &self.media_error {
                problem_box(
                    ui,
                    theme::DANGER,
                    "This media cannot be read",
                    err,
                );
            }
            return;
        };

        section(ui, "Which edition");

        // A dropdown rather than a list of radios, because the list is as long as the
        // media says: four on server media, eleven on the Windows 11 22H2 retail ISO,
        // exactly one on the Windows 10 evaluation. Eleven radios pushed the password
        // field off the first screen, and this stays one row tall whatever the media.
        let selected = self
            .chosen_image()
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "Choose an edition".to_string());
        let mut chosen = self.draft.image_index;
        if info.images.len() == 1 {
            // A picker with one option is furniture. Say what will be installed instead.
            ui.label(RichText::new(&selected).color(theme::TEXT));
        } else {
            egui::ComboBox::from_id_salt("edition")
                .selected_text(&selected)
                .width(360.0)
                .show_ui(ui, |ui| {
                    for image in &info.images {
                        ui.selectable_value(
                            &mut chosen,
                            Some(image.index),
                            &image.name,
                        );
                    }
                });
        }
        self.draft.image_index = chosen;

        // The release is a fact read off the media, so it is stated rather than offered.
        // It belongs here because this is the one place it changes what gets built.
        let mut note = format!(
            "{}, read from the media.",
            info.release
                .map(|r| r.label().to_string())
                .unwrap_or_else(|| "An unrecognised Windows".to_string())
        );
        // Said once, under the list, rather than as a tag on every row: on server media
        // half the images are Core and the distinction is the whole reason the picker is
        // here, but on client media there is no Core image at all and the note would be
        // noise.
        if info.images.iter().any(wim::is_core_image) {
            note.push_str(
                " Editions ending in CORE have no graphical desktop, command line and \
                 remote management only, and Remote Desktop is of little use on one.",
            );
        }
        hint(ui, &note);

        // Everything wrong with this media, and with this choice on it. The release
        // warning is here rather than beside a picker because there is no picker to put
        // it beside any more.
        let mut notes: Vec<String> =
            info.problems().into_iter().map(|p| p.message).collect();
        if let Some(image) = self.chosen_image() {
            notes.extend(
                media::problems_for_image(image, self.draft.enable_rdp)
                    .into_iter()
                    .map(|p| p.message),
            );
        }
        if !self.draft.release.verified_on_hardware() {
            notes.push(format!(
                "{} has not been installed on an Oxide rack by anyone yet. It is built \
                 the same way Server 2022 is, but you would be the first to try it.",
                self.draft.release.label()
            ));
        }
        for note in notes {
            ui.add_space(4.0);
            hint(ui, &note);
        }
    }

    /// The image the user picked, resolved against the media.
    pub(crate) fn chosen_image(&self) -> Option<&wim::Image> {
        let info = self.media.as_ref()?;
        let index = self.draft.image_index?;
        info.images.iter().find(|i| i.index == index)
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
                // The media's own image list, in place of what used to be two radio
                // groups the user had to get right unaided: which release this is, and
                // Desktop against Core. The release is a fact read off the ISO, and
                // Core-ness is a property of the row picked, not a separate switch.
                self.ui_edition_picker(ui);

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
                        "The installed machine takes a random computer name rather than \
                         a fixed one.",
                    );
                    hint(
                        ui,
                        "Once the install finishes, the machine runs sysprep and shuts \
                         itself down. That is what makes it cloneable: without it every \
                         clone would keep this machine's name and SID.",
                    );
                    hint(
                        ui,
                        "So a golden image ends powered off, on purpose. Take the \
                         snapshot then. Clones do not sysprep themselves again.",
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
                        "RDP also needs a VPC firewall rule allowing tcp/3389, the default VPC \
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
                    "Each of these is a complete route on its own, rather than a \
                     step in a sequence. Saving the file needs no network at all; \
                     the others use the login you already have.",
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
                self.ui_upload(ui, &artifact);
                ui.add_space(12.0);
            });
    }

    /// The upload path: pick a login, name things, and go.
    ///
    /// No disabled button with an unexplained reason — whatever is missing is said next
    /// to the control, because the reason may otherwise be scrolled out of sight.
    fn ui_upload(&mut self, ui: &mut Ui, artifact: &Path) {
        use crate::app::{Upload, UploadGoal};

        if self.profiles.is_empty() {
            hint(
                ui,
                "No Oxide login found on this machine. Run `oxide auth login` and \
                 restart, or set OXIDE_HOST and OXIDE_TOKEN. The commands below work \
                 either way.",
            );
            self.ui_upload_commands(ui, artifact);
            return;
        }

        hint(
            ui,
            "Uses the login you already have from `oxide auth login`. The image is \
             uploaded with a 512-byte block size, which is required rather than \
             preferred: this image's partition table is laid out in 512-byte sectors.",
        );

        // A ComboBox rather than radio buttons: the number of logins is data, and a
        // control sized by data pushes everything below it around.
        ui.horizontal(|ui| {
            ui.label("Rack");
            let selected = self
                .profiles
                .get(self.profile)
                .map(|p| format!("{} — {}", p.name, p.host))
                .unwrap_or_else(|| "none".into());
            egui::ComboBox::from_id_salt("profile")
                .selected_text(selected)
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (i, profile) in self.profiles.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.profile,
                            i,
                            format!("{} — {}", profile.name, profile.host),
                        );
                    }
                });
        });

        // An expired token fails as a 401 on the first request, which reads as the rack
        // being broken rather than as a login having lapsed.
        if self.profiles.get(self.profile).is_some_and(|p| p.is_expired_now()) {
            ui.label(
                RichText::new(
                    "This login has expired — run `oxide auth login` again.",
                )
                .color(theme::WARNING)
                .size(12.0),
            );
        }

        ui.horizontal(|ui| {
            ui.label("Project");
            ui.add(
                egui::TextEdit::singleline(&mut self.project)
                    .desired_width(180.0),
            );
            // A golden run derives its installer disk from the run name, so this
            // field would sit there doing nothing -- which is worse than absent,
            // because someone will type in it and expect it to matter.
            if self.upload_goal != UploadGoal::GoldenImage {
                ui.add_space(12.0);
                ui.label("Disk name");
                ui.add(
                    egui::TextEdit::singleline(&mut self.disk_name)
                        .desired_width(180.0),
                );
            }
        });

        // Three alternatives, one row. Radio buttons rather than a ComboBox because
        // the count is fixed at three rather than sized by data, and rather than two
        // checkboxes because choosing one must un-choose the others.
        let blocked = golden_unavailable(self.built_golden);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.upload_goal,
                UploadGoal::DiskOnly,
                "Disk only",
            );
            ui.selectable_value(
                &mut self.upload_goal,
                UploadGoal::WholeInstance,
                "Disk + instance",
            );
            // Disabled rather than hidden, so it is discoverable, with the reason
            // said next to it rather than left to be guessed.
            ui.add_enabled_ui(blocked.is_none(), |ui| {
                ui.selectable_value(
                    &mut self.upload_goal,
                    UploadGoal::GoldenImage,
                    "Golden image",
                );
            });
        });
        if let Some(reason) = blocked {
            // And if the selection is no longer legal -- they picked golden, then
            // went back and rebuilt as a named machine -- do not silently run the
            // wrong thing.
            if self.upload_goal == UploadGoal::GoldenImage {
                self.upload_goal = UploadGoal::DiskOnly;
            }
            ui.label(RichText::new(reason).color(theme::TEXT_DIM).size(11.5));
        }

        if self.upload_goal == UploadGoal::GoldenImage {
            self.ui_golden_controls(ui);
        }

        let whole = self.upload_goal == UploadGoal::WholeInstance;
        if whole {
            ui.horizontal(|ui| {
                ui.label("Instance");
                ui.add(
                    egui::TextEdit::singleline(&mut self.instance_name)
                        .desired_width(180.0),
                );
                ui.add_space(12.0);
                ui.label("System disk GiB");
                ui.add(
                    egui::TextEdit::singleline(&mut self.system_disk_gib)
                        .desired_width(70.0),
                );
            });
            hint(
                ui,
                "The installer is pinned as the boot disk. It is safe to leave \
                 attached — the media boots the installed Windows once there is one.",
            );
        }

        ui.add_space(8.0);
        match &self.upload {
            Upload::Running(run) => {
                let golden = self.upload_goal == UploadGoal::GoldenImage;
                // Over an hour the phase matters more than the fraction, which is
                // only meaningful while the upload is running. So the phase and the
                // elapsed time lead, and the bar appears only when there is
                // genuinely a fraction to show.
                ui.label(
                    RichText::new(format!(
                        "{} — {}",
                        run.phase,
                        fmt_duration(run.started.elapsed())
                    ))
                    .color(theme::PRIMARY)
                    .size(12.5),
                );
                match run.fraction {
                    Some(fraction) => {
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(420.0)
                                .text(run.detail.clone()),
                        );
                    }
                    None if !run.detail.is_empty() => {
                        ui.label(
                            RichText::new(&run.detail)
                                .color(theme::TEXT_DIM)
                                .size(11.5),
                        );
                    }
                    None => {}
                }
                if ui.button("Cancel").clicked() {
                    run.cancel.cancel();
                }
                if golden {
                    hint(
                        ui,
                        "Closing this window stops the run but loses nothing on the \
                         rack. To carry on from a terminal, or to pick it up later, \
                         run this — it continues from wherever it got to:",
                    );
                    code_block(
                        ui,
                        &golden_command(
                            artifact,
                            &self.project,
                            &self.run_name,
                            self.keep,
                            self.verify_clone,
                        ),
                    );
                } else {
                    hint(
                        ui,
                        "Cancelling stops the import cleanly. Closing the app instead \
                         leaves the disk mid-import, where it refuses deletion until it is \
                         stopped and finalized.",
                    );
                }
            }
            Upload::Done { outcome, elapsed } if outcome.image.is_some() => {
                let image = outcome.image.as_deref().unwrap_or_default();
                ui.label(
                    RichText::new(format!(
                        "Image {image} is ready — {}",
                        fmt_duration(*elapsed)
                    ))
                    .color(theme::PRIMARY)
                    .size(12.0),
                );
                if !outcome.leftovers.is_empty() {
                    // Not a failure. Say so, or someone goes looking for a problem
                    // with an image that is fine.
                    hint(
                        ui,
                        "The image is finished. These could not be tidied away:",
                    );
                    for command in &outcome.leftovers {
                        code_block(ui, command);
                    }
                }
            }
            Upload::Done { outcome, elapsed } => {
                ui.label(
                    RichText::new(format!(
                        "Uploaded {} in {} — {:.2} GiB sent, {:.2} GiB of zeroes \
                         skipped",
                        outcome.disk,
                        fmt_duration(*elapsed),
                        outcome.sent as f64 / 1073741824.0,
                        outcome.skipped as f64 / 1073741824.0
                    ))
                    .color(theme::PRIMARY)
                    .size(12.0),
                );
                if let Some(instance) = &outcome.instance {
                    ui.label(
                        RichText::new(format!(
                            "Instance {instance} created and starting"
                        ))
                        .color(theme::PRIMARY)
                        .size(12.0),
                    );
                }
                for warning in &outcome.warnings {
                    ui.label(
                        RichText::new(warning).color(theme::WARNING).size(11.5),
                    );
                }
            }
            Upload::Failed(failure) => {
                ui.label(
                    RichText::new(&failure.message)
                        .color(theme::DANGER)
                        .size(12.0),
                );
                if !failure.leftovers.is_empty() {
                    hint(
                        ui,
                        "These were created and have been left in place, so a long \
                         upload is not thrown away by a later failure:",
                    );
                    for command in &failure.leftovers {
                        code_block(ui, command);
                    }
                }
                if self.upload_goal == UploadGoal::GoldenImage {
                    hint(
                        ui,
                        "Nothing has been deleted. Pressing this again carries on \
                         from wherever it got to rather than starting over, and so \
                         does this command:",
                    );
                    code_block(
                        ui,
                        &golden_command(
                            artifact,
                            &self.project,
                            &self.run_name,
                            self.keep,
                            self.verify_clone,
                        ),
                    );
                    if ui.button("Carry on").clicked() {
                        self.start_golden();
                    }
                } else if ui.button("Try again").clicked() {
                    self.start_upload();
                }
            }
            Upload::Idle if self.upload_goal == UploadGoal::GoldenImage => {
                let ready = !self.project.trim().is_empty()
                    && !self.run_name.trim().is_empty();
                if ready {
                    if ui.button("Build the golden image").clicked() {
                        self.start_golden();
                    }
                } else {
                    ui.label(
                        RichText::new(
                            "Name a project and a run to start the golden build.",
                        )
                        .color(theme::TEXT_DIM)
                        .size(12.0),
                    );
                }
            }
            Upload::Idle => {
                let ready = !self.project.trim().is_empty()
                    && !self.disk_name.trim().is_empty();
                if ready {
                    if ui.button("Upload to the rack").clicked() {
                        self.start_upload();
                    }
                } else {
                    // Say what is wrong rather than showing a dead button.
                    ui.label(
                        RichText::new(
                            "Name a project and a disk to enable the upload.",
                        )
                        .color(theme::TEXT_DIM)
                        .size(12.0),
                    );
                }
            }
        }

        ui.add_space(8.0);
        ui.collapsing("Or run it yourself", |ui| {
            self.ui_upload_commands(ui, artifact);
        });
    }

    /// The three controls a golden run needs, and the one warning it deserves.
    fn ui_golden_controls(&mut self, ui: &mut Ui) {
        hint(
            ui,
            "Installs Windows once, generalizes it, and turns the result into an \
             image the rack can stamp copies from. About an hour, almost all of it \
             waiting for Setup.",
        );
        ui.horizontal(|ui| {
            ui.label("Run name");
            ui.add(
                egui::TextEdit::singleline(&mut self.run_name)
                    .desired_width(180.0),
            );
            ui.add_space(12.0);
            ui.label("Keep");
            egui::ComboBox::from_id_salt("keep")
                .selected_text(keep_label(self.keep))
                .width(190.0)
                .show_ui(ui, |ui| {
                    for keep in [
                        oxwin_rack::Keep::Image,
                        oxwin_rack::Keep::Snapshot,
                        oxwin_rack::Keep::Disks,
                        oxwin_rack::Keep::All,
                    ] {
                        ui.selectable_value(
                            &mut self.keep,
                            keep,
                            keep_label(keep),
                        );
                    }
                });
        });
        hint(
            ui,
            "Everything is named from the run name: <run>-installer, <run>-system, \
             the instance <run>, <run>-snap, and the image <run>.",
        );
        ui.checkbox(
            &mut self.verify_clone,
            "Prove it boots: make a clone and wait for it to come up and stay up",
        );
        if self.verify_clone {
            hint(
                ui,
                "Adds about ten minutes, and leaves the clone running so you can log \
                 in and check its computer name differs from the original. That is \
                 the one thing this cannot check for you.",
            );
        }
    }

    /// The copyable commands. Still here, and still complete: an airgapped rack, or
    /// someone else doing the upload, must not depend on this app reaching a network.
    fn ui_upload_commands(&mut self, ui: &mut Ui, artifact: &Path) {
        let cmd = upload_command(artifact, &self.project, &self.disk_name);
        code_block(ui, &cmd);
        if ui.button("Copy command").clicked() {
            ui.ctx().copy_text(cmd);
            self.notice = Some("Command copied.".into());
        }
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
/// One line naming what the media turned out to be.
///
/// The release is stated as a fact because it was read out of the media, not chosen —
/// the point of saying it here is that a user who dropped the wrong ISO sees so at once.
fn describe_detection(info: &MediaInfo) -> String {
    let release = match (info.release, info.build_recognised) {
        (Some(r), true) => r.label().to_string(),
        (Some(r), false) => format!("{} (probably)", r.label()),
        (None, _) => "an unrecognised Windows".to_string(),
    };
    format!(
        "{release} · {} edition{} · {} media",
        info.images.len(),
        if info.images.len() == 1 { "" } else { "s" },
        if info.is_evaluation() { "evaluation" } else { "retail or volume" },
    )
}

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

/// The command that runs, or resumes, a golden build.
///
/// Shown while a run is in flight and again if it fails, because **closing this
/// window kills the run but loses nothing on the rack** — every step asks the rack
/// what already exists, so continuing is running this line. An hour is long enough
/// that someone will close the laptop, and the honest answer is to hand them the
/// command rather than to pretend the window is safe.
fn golden_command(
    image: &Path,
    project: &str,
    run: &str,
    keep: oxwin_rack::Keep,
    verify: bool,
) -> String {
    let mut cmd = format!(
        "oxwin golden {} \\\n  --run {run} \\\n  --project {project}",
        image.display()
    );
    // Only when it is not the default: a command someone copies should be the
    // shortest one that does what they asked for.
    if keep != oxwin_rack::Keep::default() {
        cmd.push_str(&format!(" \\\n  --keep={}", keep_flag(keep)));
    }
    if verify {
        cmd.push_str(" \\\n  --verify-clone");
    }
    cmd
}

/// What each level means, rather than what it is called. A picker that says only
/// "disks" makes someone guess whether that is what survives or what goes.
fn keep_label(keep: oxwin_rack::Keep) -> &'static str {
    match keep {
        oxwin_rack::Keep::Image => "the image",
        oxwin_rack::Keep::Snapshot => "the image and snapshot",
        oxwin_rack::Keep::Disks => "everything but the instance",
        oxwin_rack::Keep::All => "everything",
    }
}

fn keep_flag(keep: oxwin_rack::Keep) -> &'static str {
    match keep {
        oxwin_rack::Keep::Image => "image",
        oxwin_rack::Keep::Snapshot => "snapshot",
        oxwin_rack::Keep::Disks => "disks",
        oxwin_rack::Keep::All => "all",
    }
}

/// Why the golden option cannot be chosen, or `None` if it can.
///
/// Keyed on what this session actually *built*, never on the current draft: the
/// draft stays editable after the build, so reading it here would offer a golden run
/// for an image that is not one. That image installs perfectly and never shuts down,
/// and the watcher cannot tell that apart from a hang — it would spend two hours on a
/// working install before saying so.
fn golden_unavailable(built_golden: Option<bool>) -> Option<&'static str> {
    match built_golden {
        Some(true) => None,
        Some(false) => Some(
            "This image was built as a named machine, so it will not generalize \
             itself. Go back to Settings, choose Golden image, and build again.",
        ),
        None => Some(
            "Only available for an image built in this session, because nothing in \
             an image file says whether it was built to generalize.",
        ),
    }
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
    fn the_golden_command_is_the_resume_command() {
        let cmd = golden_command(
            Path::new("/tmp/g4.img"),
            "danb",
            "g4",
            oxwin_rack::Keep::Image,
            false,
        );
        assert!(cmd.contains("oxwin golden /tmp/g4.img"), "{cmd}");
        assert!(cmd.contains("--run g4"), "{cmd}");
        assert!(cmd.contains("--project danb"), "{cmd}");
        // The default is not spelled out: a command someone copies should be the
        // shortest one that does what they asked for.
        assert!(!cmd.contains("--keep"), "{cmd}");
        assert!(!cmd.contains("--verify-clone"), "{cmd}");
    }

    #[test]
    fn a_non_default_keep_and_verify_are_spelled_out() {
        let cmd = golden_command(
            Path::new("/tmp/g4.img"),
            "danb",
            "g4",
            oxwin_rack::Keep::All,
            true,
        );
        assert!(cmd.contains("--keep=all"), "{cmd}");
        assert!(cmd.contains("--verify-clone"), "{cmd}");
    }

    /// Every level has a flag spelling, so a picker cannot produce a command that
    /// does something other than what the picker said.
    #[test]
    fn every_keep_level_has_a_flag() {
        for keep in [
            oxwin_rack::Keep::Image,
            oxwin_rack::Keep::Snapshot,
            oxwin_rack::Keep::Disks,
            oxwin_rack::Keep::All,
        ] {
            let flag = keep_flag(keep);
            assert_eq!(
                flag.parse::<oxwin_rack::Keep>().unwrap(),
                keep,
                "{flag} does not round-trip"
            );
        }
    }

    /// The gate reads what was built, not what the draft currently says. Offering a
    /// golden run for media built as a named machine produces an install that works
    /// perfectly and never shuts down.
    #[test]
    fn golden_is_offered_only_for_an_image_built_as_one() {
        assert_eq!(golden_unavailable(Some(true)), None);
        let named = golden_unavailable(Some(false)).expect("a reason");
        assert!(named.contains("named machine"), "{named}");
        // And it says how to fix it, rather than only that it is wrong.
        assert!(named.contains("Settings"), "{named}");
        assert!(golden_unavailable(None).is_some());
    }

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
