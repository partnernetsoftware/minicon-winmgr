//! Pure four-edge docking geometry; shared by AppBar and fallback work areas.
use crate::config::Position;
use windows::Win32::Foundation::RECT;
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
