// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The stage indicator across the top: a circle per stage, a label under each,
//! and a connecting line between them. Grey until a stage is done, then green.
//!
//! Drawn rather than assembled from widgets, because the shape is specific and
//! painting it directly is less code than bending a layout to it.

use crate::theme::{GREY, PRIMARY, SECONDARY, TEXT, TEXT_DIM};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Done,
    Current,
    Todo,
}

const RADIUS: f32 = 15.0;
const ROW_HEIGHT: f32 = 74.0;

/// Draw the stepper and return the index of any stage the user clicked, so
/// completed stages can be revisited. A build that takes ten minutes must not
/// force a restart just because someone wants to change the hostname.
pub fn show(
    ui: &mut Ui,
    labels: &[&str],
    state: impl Fn(usize) -> State,
    clickable: impl Fn(usize) -> bool,
) -> Option<usize> {
    let n = labels.len();
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), ROW_HEIGHT),
        Sense::hover(),
    );
    let painter = ui.painter();

    let slot = rect.width() / n as f32;
    let cy = rect.top() + RADIUS + 6.0;
    let centre =
        |i: usize| Pos2::new(rect.left() + slot * (i as f32 + 0.5), cy);

    // Connectors first, so the circles sit on top of them.
    for i in 0..n.saturating_sub(1) {
        let a = centre(i);
        let b = centre(i + 1);
        // A connector is green only once the stage it leads *out of* is done.
        let colour = if state(i) == State::Done { PRIMARY } else { GREY };
        painter.line_segment(
            [
                Pos2::new(a.x + RADIUS + 4.0, cy),
                Pos2::new(b.x - RADIUS - 4.0, cy),
            ],
            Stroke::new(2.0, colour),
        );
    }

    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let c = centre(i);
        match state(i) {
            State::Done => {
                painter.circle_filled(c, RADIUS, PRIMARY);
                check(ui, c);
            }
            State::Current => {
                painter.circle_filled(c, RADIUS, SECONDARY);
                painter.circle_stroke(c, RADIUS, Stroke::new(2.0, PRIMARY));
                painter.circle_filled(c, 4.5, PRIMARY);
            }
            State::Todo => {
                painter.circle_filled(c, RADIUS, GREY);
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    FontId::proportional(12.0),
                    TEXT_DIM,
                );
            }
        }

        let colour = match state(i) {
            State::Done | State::Current => TEXT,
            State::Todo => TEXT_DIM,
        };
        painter.text(
            Pos2::new(c.x, cy + RADIUS + 12.0),
            Align2::CENTER_TOP,
            *label,
            FontId::proportional(12.5),
            colour,
        );

        // Hit-test the whole slot, not just the circle, so the label is clickable too.
        if clickable(i) {
            let hit = Rect::from_min_max(
                Pos2::new(c.x - slot / 2.0, rect.top()),
                Pos2::new(c.x + slot / 2.0, rect.bottom()),
            );
            let r =
                ui.interact(hit, ui.id().with(("stepper", i)), Sense::click());
            if r.clicked() {
                clicked = Some(i);
            }
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    }

    clicked
}

/// A check mark painted from two strokes. The default font set cannot be relied
/// on for a tick glyph, and a missing glyph in the one place that signals success
/// would be a poor trade for three lines of code.
fn check(ui: &Ui, c: Pos2) {
    let p = ui.painter();
    let stroke = Stroke::new(2.2, Color32::from_rgb(0x04, 0x1a, 0x14));
    let a = Pos2::new(c.x - 5.5, c.y + 0.5);
    let b = Pos2::new(c.x - 1.5, c.y + 4.5);
    let d = Pos2::new(c.x + 6.0, c.y - 4.0);
    p.line_segment([a, b], stroke);
    p.line_segment([b, d], stroke);
}
