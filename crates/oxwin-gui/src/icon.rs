// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! The window and dock icon, drawn rather than embedded.
//!
//! A `.png` committed next to the source is a blob nobody can review: it arrives in a
//! diff as "binary files differ", its provenance rests on whoever exported it, and it is
//! the one artifact in this repository that cannot be checked by reading. Forty lines of
//! arithmetic can be.
//!
//! The mark is three stacked bars, brightening upwards. That is the thing being built:
//! the image really is a stack — protective MBR, the FAT32 ESP that firmware boots, and
//! the exFAT volume carrying Windows — and it is assembled bottom up. It also survives
//! being 16 pixels wide in a taskbar, which a wordmark or anything with fine detail does
//! not.

use crate::theme;

/// Edge of the rendered icon, in pixels. 128 is what macOS, Windows and the X11/Wayland
/// hint all downsample from happily; going larger buys nothing at the sizes a window
/// icon is ever drawn at.
const SIZE: usize = 128;

/// Supersampling factor. Every shape below is drawn with hard edges — a pixel is inside
/// the rectangle or it is not — and the box filter on the way down is what turns that
/// into smooth edges. Antialiasing the shapes directly would mean coverage arithmetic in
/// every primitive, for a worse result.
const SS: usize = 4;

/// The design grid every constant below is written on. [`rgba`] scales it to the
/// requested edge, so 128 is a unit of measure here and not a resolution.
const GRID: usize = 128;

/// The icon, ready for `ViewportBuilder::with_icon`.
pub fn icon() -> egui::IconData {
    egui::IconData { rgba: rgba(SIZE), width: SIZE as u32, height: SIZE as u32 }
}

/// The same mark at any edge, as RGBA8.
///
/// macOS wants an `.icns` holding everything from 16 to 1024 pixels square, and
/// upscaling the 128-pixel window icon to 1024 would ship a blurry Dock icon. So
/// the renderer is resolution-independent and the bundle's icon is generated from
/// this same arithmetic — see `examples/icon-png.rs` and `tools/package-macos.sh`.
/// Committing a `.png` instead would put the one unreviewable blob in the
/// repository, which is what the note at the top of this file is about.
pub fn rgba(edge: usize) -> Vec<u8> {
    let mut canvas = Canvas::new(edge * SS, edge);

    // Scale the design grid to the requested size. Integer arithmetic throughout:
    // at the sizes an icon is drawn, a rounding difference is a pixel, and the
    // supersampling below is what hides it.
    let u = |n: usize| n * edge * SS / GRID;

    // The plate. Not the app background: at this size a near-black square vanishes into
    // a dark dock, so the icon needs to be a shade the surrounding chrome is not.
    canvas.rounded_rect(0, 0, u(128), u(128), u(24), theme::SURFACE);

    // Three bars, dim to bright bottom to top. The gradient is the whole message — the
    // stack is being built, and the top of it is the live one.
    let shades = [
        theme::PRIMARY.gamma_multiply(0.30),
        theme::PRIMARY.gamma_multiply(0.60),
        theme::PRIMARY,
    ];
    for (row, shade) in shades.iter().enumerate() {
        let y = u(86 - row * 30);
        canvas.rounded_rect(u(26), y, u(76), u(16), u(4), *shade);
    }

    canvas.downsample()
}

/// A square RGBA buffer at `SS` times the final resolution.
struct Canvas {
    pixels: Vec<u8>,
    edge: usize,
    /// Edge of the buffer [`Canvas::downsample`] produces: `edge / SS`, held rather
    /// than divided out so the two cannot disagree.
    out_edge: usize,
}

impl Canvas {
    fn new(edge: usize, out_edge: usize) -> Self {
        Self { pixels: vec![0u8; edge * edge * 4], edge, out_edge }
    }

    /// Fill an axis-aligned rounded rectangle, opaque, no antialiasing —
    /// [`Canvas::downsample`] supplies that.
    fn rounded_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        color: egui::Color32,
    ) {
        // A radius larger than half the shorter side has no meaning and would make the
        // corner tests below overlap, so clamp rather than trusting the caller.
        let radius = radius.min(w / 2).min(h / 2);
        let r2 = (radius * radius) as i64;
        for row in 0..h {
            for col in 0..w {
                // Distance from the corner circle's centre, but only within the corner
                // squares; everywhere else is unconditionally inside.
                let dx = if col < radius {
                    (radius - col) as i64
                } else if col >= w - radius {
                    (col - (w - radius) + 1) as i64
                } else {
                    0
                };
                let dy = if row < radius {
                    (radius - row) as i64
                } else if row >= h - radius {
                    (row - (h - radius) + 1) as i64
                } else {
                    0
                };
                if dx * dx + dy * dy > r2 {
                    continue;
                }
                let i = ((y + row) * self.edge + (x + col)) * 4;
                self.pixels[i] = color.r();
                self.pixels[i + 1] = color.g();
                self.pixels[i + 2] = color.b();
                self.pixels[i + 3] = 255;
            }
        }
    }

    /// Box-filter `SS`×`SS` blocks down to one pixel each.
    fn downsample(&self) -> Vec<u8> {
        let n = (SS * SS) as u32;
        let out_edge = self.out_edge;
        let mut out = vec![0u8; out_edge * out_edge * 4];
        for y in 0..out_edge {
            for x in 0..out_edge {
                let mut sums = [0u32; 4];
                for sy in 0..SS {
                    for sx in 0..SS {
                        let i = ((y * SS + sy) * self.edge + (x * SS + sx)) * 4;
                        for (channel, sum) in sums.iter_mut().enumerate() {
                            *sum += u32::from(self.pixels[i + channel]);
                        }
                    }
                }
                let o = (y * out_edge + x) * 4;
                for (channel, sum) in sums.iter().enumerate() {
                    out[o + channel] = (sum / n) as u8;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `with_icon` takes the buffer on trust: a length that disagrees with the declared
    /// dimensions is read past the end or drawn as garbage, depending on the platform.
    #[test]
    fn the_buffer_matches_the_declared_size() {
        let icon = icon();
        assert_eq!(icon.width, SIZE as u32);
        assert_eq!(icon.height, SIZE as u32);
        assert_eq!(icon.rgba.len(), SIZE * SIZE * 4);
    }

    /// The failure mode worth catching is not an ugly icon, it is an empty one — a
    /// drawing bug that leaves every pixel transparent still produces a valid buffer and
    /// a window with no icon at all, which looks exactly like not setting one.
    #[test]
    fn it_is_not_blank() {
        let icon = icon();
        let opaque = icon.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
        assert!(
            opaque > SIZE * SIZE / 2,
            "only {opaque} of {} pixels are opaque; the plate should cover nearly \
             all of them",
            SIZE * SIZE
        );

        // And that the mark is actually on the plate. Sampling the middle of the top
        // bar against the gap below it catches a stack drawn in one flat colour, which
        // the opacity check above cannot see.
        let at = |x: usize, y: usize| {
            let i = (y * SIZE + x) * 4;
            [icon.rgba[i], icon.rgba[i + 1], icon.rgba[i + 2]]
        };
        assert_ne!(
            at(64, 34),
            at(64, 50),
            "top bar is not distinct from the gap"
        );
        assert_ne!(at(64, 34), at(64, 94), "the bars are all the same shade");
    }

    /// The `.icns` in the macOS bundle is rendered at every size Apple asks for, by
    /// a packaging script nobody runs on a laptop. An off-by-one in the scaling
    /// arithmetic would surface as a panic in CI at release time, or — worse, since
    /// it cannot panic — as a blank icon at one size only.
    #[test]
    fn it_renders_at_every_size_the_bundle_needs() {
        for edge in [16usize, 32, 64, 128, 256, 512, 1024] {
            let rgba = rgba(edge);
            assert_eq!(
                rgba.len(),
                edge * edge * 4,
                "{edge}px buffer is the wrong length"
            );
            let opaque = rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
            assert!(
                opaque > edge * edge / 2,
                "the {edge}px icon is mostly transparent ({opaque} of {} pixels \
                 opaque), so the plate did not scale",
                edge * edge
            );

            // The mark has to survive the scaling too, not just the plate. Sampled
            // on the design grid so the same two points are compared at every size.
            let at = |x: usize, y: usize| {
                let i = ((y * edge / GRID) * edge + (x * edge / GRID)) * 4;
                [rgba[i], rgba[i + 1], rgba[i + 2]]
            };
            // 16px is below the resolution at which three bars and two gaps are
            // distinguishable at all; the plate check above is what it gets.
            if edge >= 32 {
                assert_ne!(
                    at(64, 34),
                    at(64, 50),
                    "at {edge}px the top bar merged into the gap below it"
                );
            }
        }
    }
}
