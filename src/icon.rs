//! The colored-calendar glyph, drawn in-process and shared by both icon
//! surfaces: the system-tray indicator (`tray.rs`, which repacks to ARGB32 for
//! ksni) and the widget window / app icon (`app.rs`, via `iced::window::icon`).
//!
//! Producing one glyph here keeps the tray and the dock/taskbar icon identical.
//! Output is RGBA8, row-major, so callers convert to whatever their API wants.

type Rgb = (u8, u8, u8);

const WHITE: Rgb = (255, 255, 255);
const BLUE: Rgb = (66, 133, 244); // calendar-blue header
const DARK: Rgb = (60, 64, 67); // binding tabs
const RED: Rgb = (234, 67, 53); // "today" accent dot
const GREY: Rgb = (154, 160, 166); // other day dots

/// Set one opaque pixel (RGBA8: R, G, B, A).
fn set_px(data: &mut [u8], s: i32, x: i32, y: i32, (r, g, b): Rgb) {
    if x < 0 || y < 0 || x >= s || y >= s {
        return;
    }
    let i = ((y * s + x) as usize) * 4;
    data[i] = r;
    data[i + 1] = g;
    data[i + 2] = b;
    data[i + 3] = 255;
}

/// Fill a rectangle whose corners can be individually rounded
/// (`corners` = [top-left, top-right, bottom-left, bottom-right]).
#[allow(clippy::too_many_arguments)]
fn fill_rounded(
    data: &mut [u8],
    s: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    r: i32,
    corners: [bool; 4],
    color: Rgb,
) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            // Skip a pixel only if it's outside the rounding circle of a corner
            // that is meant to be rounded.
            let outside = |cx: i32, cy: i32| {
                let (dx, dy) = ((x - cx) as f32, (y - cy) as f32);
                dx * dx + dy * dy > (r * r) as f32
            };
            let skip = (corners[0] && x < x0 + r && y < y0 + r && outside(x0 + r, y0 + r))
                || (corners[1] && x > x1 - r && y < y0 + r && outside(x1 - r, y0 + r))
                || (corners[2] && x < x0 + r && y > y1 - r && outside(x0 + r, y1 - r))
                || (corners[3] && x > x1 - r && y > y1 - r && outside(x1 - r, y1 - r));
            if !skip {
                set_px(data, s, x, y, color);
            }
        }
    }
}

/// Fill a solid disc centered at (cx, cy).
fn fill_disc(data: &mut [u8], s: i32, cx: i32, cy: i32, r: i32, color: Rgb) {
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let (dx, dy) = ((x - cx) as f32, (y - cy) as f32);
            if dx * dx + dy * dy <= (r * r) as f32 {
                set_px(data, s, x, y, color);
            }
        }
    }
}

/// Draw the colored calendar glyph at `size`×`size` as RGBA8 pixels: a rounded
/// white card with a blue header band, two dark binding tabs, and a grid of day
/// dots (the first one red as a "today" accent). Fully transparent background.
pub fn calendar_rgba(size: u32) -> Vec<u8> {
    let s = size as i32;
    let sf = size as f32;
    let mut data = vec![0u8; (size * size * 4) as usize];

    let m = (sf * 0.09).round() as i32;
    let card_left = m;
    let card_right = s - m;
    let card_top = (sf * 0.22).round() as i32;
    let card_bottom = s - m;
    let radius = ((sf * 0.12).round() as i32).max(1);
    let header_bottom = card_top + ((card_bottom - card_top) as f32 * 0.30) as i32;

    // Binding tabs peeking above the card.
    let tab_w = ((sf * 0.09).round() as i32).max(1);
    let tab_top = (sf * 0.06).round() as i32;
    for tab_x in [
        card_left + (sf * 0.16) as i32,
        card_right - (sf * 0.16) as i32 - tab_w,
    ] {
        fill_rounded(
            &mut data,
            s,
            tab_x,
            tab_top,
            tab_x + tab_w,
            header_bottom,
            (tab_w / 2).max(1),
            [true, true, false, false],
            DARK,
        );
    }

    // Blue card (rounded), then the white body below the header (bottom corners
    // rounded, top square so it meets the header on a straight line).
    fill_rounded(
        &mut data,
        s,
        card_left,
        card_top,
        card_right,
        card_bottom,
        radius,
        [true, true, true, true],
        BLUE,
    );
    fill_rounded(
        &mut data,
        s,
        card_left,
        header_bottom,
        card_right,
        card_bottom,
        radius,
        [false, false, true, true],
        WHITE,
    );

    // Day dots: 3 columns × 2 rows across the white body; first one red.
    let dot_r = ((sf * 0.055).round() as i32).max(1);
    let body_left = card_left + (sf * 0.13) as i32;
    let body_right = card_right - (sf * 0.13) as i32;
    let body_top = header_bottom + (sf * 0.12) as i32;
    let col_gap = (body_right - body_left) / 2;
    let row_gap = (card_bottom - (sf * 0.10) as i32 - body_top).max(1);
    for (idx, (col, row)) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]
        .into_iter()
        .enumerate()
    {
        let cx = body_left + col * col_gap;
        let cy = body_top + row * row_gap;
        let color = if idx == 0 { RED } else { GREY };
        fill_disc(&mut data, s, cx, cy, dot_r, color);
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_rgba_has_expected_size_and_opaque_pixels() {
        for size in [22u32, 24, 32, 48, 64] {
            let px = calendar_rgba(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            // Some pixels are painted opaque (alpha == 255), some transparent.
            assert!(px.chunks_exact(4).any(|p| p[3] == 255));
            assert!(px.chunks_exact(4).any(|p| p[3] == 0));
        }
    }
}
