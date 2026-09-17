// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The palette, in one place.

use egui::Color32;

/// Oxide green. Used for completed stages, focus, and the primary action.
pub const PRIMARY: Color32 = Color32::from_rgb(0x00, 0xb7, 0x7d);
/// Deep green, for fills that sit behind the primary colour.
pub const SECONDARY: Color32 = Color32::from_rgb(0x00, 0x29, 0x23);
/// The grey that segments things: incomplete stages, dividers, control fills.
pub const GREY: Color32 = Color32::from_rgb(0x29, 0x2c, 0x2f);
pub const BACKGROUND: Color32 = Color32::from_rgb(0x0b, 0x0e, 0x12);

/// Derived neutrals. Not in the brand list, but text needs to be legible and
/// warnings need to be visible, so they are defined here rather than inline.
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xed, 0xf0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x93, 0x9c);
pub const WARNING: Color32 = Color32::from_rgb(0xe3, 0xb3, 0x41);
pub const DANGER: Color32 = Color32::from_rgb(0xe0, 0x5d, 0x5d);
/// One step up from the background, for panels and the log pane.
pub const SURFACE: Color32 = Color32::from_rgb(0x11, 0x16, 0x1c);

/// Corner radius used everywhere. Small on purpose: heavily rounded panels read as
/// consumer software, and Oxide's own interfaces are squarer than that.
pub const CORNER: u8 = 3;

pub fn apply(ctx: &egui::Context) {
    // The palette is dark by construction, so pin the theme rather than letting the
    // host OS switch us into a light style these colours were not chosen for.
    ctx.set_theme(egui::ThemePreference::Dark);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = BACKGROUND;
    visuals.extreme_bg_color = SURFACE;
    visuals.faint_bg_color = SURFACE;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = PRIMARY;
    visuals.selection.bg_fill = SECONDARY;
    visuals.selection.stroke = egui::Stroke::new(1.0, PRIMARY);

    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, GREY);
    visuals.widgets.inactive.bg_fill = GREY;
    visuals.widgets.inactive.weak_bg_fill = GREY;
    visuals.widgets.hovered.bg_fill = SECONDARY;
    visuals.widgets.hovered.weak_bg_fill = SECONDARY;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, PRIMARY);
    visuals.widgets.active.bg_fill = SECONDARY;
    visuals.widgets.active.weak_bg_fill = SECONDARY;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, PRIMARY);

    // Both stored styles get the same treatment so nothing can flip underneath us.
    ctx.all_styles_mut(|style| {
        style.visuals = visuals.clone();
        // Oxide's house style is restrained about rounding. Pin every widget to the
        // same small radius rather than leaving egui's assorted defaults in place.
        for w in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            w.corner_radius = egui::CornerRadius::same(CORNER);
        }
        style.visuals.window_corner_radius = egui::CornerRadius::same(CORNER);
        style.visuals.menu_corner_radius = egui::CornerRadius::same(CORNER);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
}
