//! Pure four-edge docking geometry; shared by AppBar and fallback work areas.
use crate::config::Position;
use windows::Win32::Foundation::RECT;

pub(crate) fn side_width_pixels(dip: i32, dpi: u32, span: i32) -> i32 {
    let scale = |value: i32| {
        ((value as i64 * dpi.max(96) as i64 + 48) / 96).clamp(1, i32::MAX as i64) as i32
    };
    let max = scale(crate::config::SIDE_WIDTH_MAX).min((span / 3).max(1));
    let min = scale(crate::config::SIDE_WIDTH_MIN).min(max);
    scale(dip.max(1)).clamp(min, max)
}

pub(crate) fn side_resize_hit(edge: Position, client: RECT, x: i32, y: i32, grip: i32) -> bool {
    y >= client.top
        && y < client.bottom
        && x >= client.left
        && x < client.right
        && match edge {
            Position::Left => x >= client.right - grip,
            Position::Right => x < client.left + grip,
            _ => false,
        }
}

pub(crate) fn dragged_side_width(edge: Position, outer: RECT, pointer_x: i32) -> i32 {
    match edge {
        Position::Left => pointer_x.saturating_sub(outer.left),
        Position::Right => outer.right.saturating_sub(pointer_x),
        _ => outer.right - outer.left,
    }
}
pub(crate) fn bounds(area: RECT, edge: Position, thickness: i32) -> RECT {
    let mut bar = area;
    let amount = thickness.max(1).min(
        if edge.vertical() {
            area.right - area.left
        } else {
            area.bottom - area.top
        }
        .max(1),
    );
    match edge {
        Position::Bottom => bar.top = area.bottom - amount,
        Position::Top => bar.bottom = area.top + amount,
        Position::Left => bar.right = area.left + amount,
        Position::Right => bar.left = area.right - amount,
    }
    bar
}
pub(crate) fn reserve(area: RECT, edge: Position, thickness: i32) -> RECT {
    let bar = bounds(area, edge, thickness);
    let mut work = area;
    match edge {
        Position::Bottom => work.bottom = bar.top,
        Position::Top => work.top = bar.bottom,
        Position::Left => work.left = bar.right,
        Position::Right => work.right = bar.left,
    }
    work
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn side_resize_respects_dpi_screen_limits_and_negative_coordinates() {
        assert_eq!(side_width_pixels(180, 96, 1920), 180);
        assert_eq!(side_width_pixels(180, 144, 1920), 270);
        assert_eq!(side_width_pixels(480, 192, 1200), 400);
        assert_eq!(side_width_pixels(180, 96, 90), 30);
        let outer = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert_eq!(dragged_side_width(Position::Left, outer, -1700), 220);
        assert_eq!(dragged_side_width(Position::Right, outer, -320), 320);
        let client = RECT {
            left: 0,
            top: 0,
            right: 180,
            bottom: 1080,
        };
        assert!(side_resize_hit(Position::Left, client, 177, 200, 6));
        assert!(!side_resize_hit(Position::Left, client, 100, 200, 6));
        assert!(side_resize_hit(Position::Right, client, 2, 200, 6));
        assert!(!side_resize_hit(Position::Top, client, 2, 200, 6));
        assert!(!side_resize_hit(Position::Right, client, 2, -1, 6));
    }
    #[test]
    fn four_edges_preserve_negative_monitor_origin_and_available_area() {
        let area = RECT {
            left: -1920,
            top: -100,
            right: 0,
            bottom: 980,
        };
        for edge in [
            Position::Bottom,
            Position::Top,
            Position::Left,
            Position::Right,
        ] {
            let bar = bounds(area, edge, 36);
            let work = reserve(area, edge, 36);
            assert!(
                bar.left >= area.left
                    && bar.top >= area.top
                    && bar.right <= area.right
                    && bar.bottom <= area.bottom
            );
            assert_eq!(
                (bar.right - bar.left) * (bar.bottom - bar.top)
                    + (work.right - work.left) * (work.bottom - work.top),
                1920 * 1080
            );
            if edge.vertical() {
                assert_eq!(bar.right - bar.left, 36);
                assert_eq!(work.bottom - work.top, 1080);
            } else {
                assert_eq!(bar.bottom - bar.top, 36);
                assert_eq!(work.right - work.left, 1920);
            }
        }
    }
}
