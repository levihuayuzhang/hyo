//! Virtualized worksheet canvas.
//!
//! Only the cells intersecting the viewport are drawn. Headers and the
//! selection are painted by the same canvas, so the grid needs no scrollable
//! container and keeps the row/column headers frozen.

use crate::app::Message;
use hyo_core::{CellAddr, Value, Workbook, addr::column_name};
use iced::alignment::Vertical;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Stroke, Text};
use iced::widget::text::Alignment as TextAlignment;
use iced::{Color, Point, Rectangle, Size, Vector};

pub const HEADER_W: f32 = 52.0;
pub const HEADER_H: f32 = 26.0;
pub const CELL_W: f32 = 108.0;
pub const CELL_H: f32 = 26.0;
const SCROLLBAR: f32 = 10.0;
const MAX_TEXT_CHARS: usize = 18;

pub const BACKGROUND: Color = Color::from_rgb(0.075, 0.086, 0.110);
pub const CELL_BG: Color = Color::from_rgb(0.094, 0.106, 0.133);
const CELL_ALT: Color = Color::from_rgb(0.101, 0.114, 0.143);
const GRID_LINE: Color = Color::from_rgb(0.165, 0.184, 0.220);
const HEADER_BG: Color = Color::from_rgb(0.110, 0.125, 0.157);
const HEADER_ACTIVE: Color = Color::from_rgb(0.160, 0.251, 0.365);
const HEADER_TEXT: Color = Color::from_rgb(0.600, 0.650, 0.720);
const TEXT: Color = Color::from_rgb(0.855, 0.890, 0.940);
const TEXT_MUTED: Color = Color::from_rgb(0.530, 0.590, 0.670);
const ERROR: Color = Color::from_rgb(1.0, 0.400, 0.400);
const ACCENT: Color = Color::from_rgb(0.180, 0.620, 0.880);
const SELECTION: Color = Color::from_rgb(0.105, 0.235, 0.357);
const SCROLLBAR_TRACK: Color = Color::from_rgb(0.086, 0.098, 0.125);
const SCROLLBAR_THUMB: Color = Color::from_rgb(0.260, 0.300, 0.360);

pub struct Grid<'a> {
    pub workbook: &'a Workbook,
    pub sheet: usize,
    pub active: CellAddr,
    pub scroll: Vector,
}

impl canvas::Program<Message> for Grid<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), BACKGROUND);

        let Some(sheet) = self.workbook.sheet(self.sheet) else {
            return vec![frame.into_geometry()];
        };

        let grid_w = (bounds.width - HEADER_W).max(1.0);
        let grid_h = (bounds.height - HEADER_H).max(1.0);
        let first_col = (self.scroll.x / CELL_W).floor().max(0.0) as u32;
        let first_row = (self.scroll.y / CELL_H).floor().max(0.0) as u32;
        let cols = (grid_w / CELL_W).ceil() as u32 + 2;
        let rows = (grid_h / CELL_H).ceil() as u32 + 2;
        let last_col = first_col.saturating_add(cols);
        let last_row = first_row.saturating_add(rows);

        // Cells (clipped so long values cannot leak under the headers).
        let clip = Rectangle {
            x: HEADER_W,
            y: HEADER_H,
            width: grid_w,
            height: grid_h,
        };
        frame.with_clip(clip, |frame| {
            for row in first_row..=last_row.min(hyo_core::MAX_ROWS - 1) {
                let y = HEADER_H + row as f32 * CELL_H - self.scroll.y;
                for col in first_col..=last_col.min(hyo_core::MAX_COLS - 1) {
                    let x = HEADER_W + col as f32 * CELL_W - self.scroll.x;
                    let addr = CellAddr::new(row, col);
                    let selected = addr == self.active;
                    let fill = if selected {
                        SELECTION
                    } else if row % 2 == 0 {
                        CELL_BG
                    } else {
                        CELL_ALT
                    };
                    frame.fill_rectangle(Point::new(x, y), Size::new(CELL_W, CELL_H), fill);
                    frame.stroke_rectangle(
                        Point::new(x, y),
                        Size::new(CELL_W, CELL_H),
                        Stroke::default().with_width(1.0).with_color(GRID_LINE),
                    );

                    let value = sheet.value(addr);
                    if !value.is_empty() {
                        let (content, color, align, position) = cell_text(&value, x, y);
                        frame.fill_text(Text {
                            content,
                            position,
                            max_width: CELL_W - 10.0,
                            color,
                            size: 13.0.into(),
                            align_x: align,
                            align_y: Vertical::Center,
                            ..Text::default()
                        });
                    }

                    if selected {
                        frame.stroke_rectangle(
                            Point::new(x + 1.0, y + 1.0),
                            Size::new(CELL_W - 2.0, CELL_H - 2.0),
                            Stroke::default().with_width(2.0).with_color(ACCENT),
                        );
                    }
                }
            }
        });

        // Column header strip.
        frame.fill_rectangle(
            Point::new(HEADER_W, 0.0),
            Size::new(grid_w, HEADER_H),
            HEADER_BG,
        );
        frame.with_clip(
            Rectangle {
                x: HEADER_W,
                y: 0.0,
                width: grid_w,
                height: HEADER_H,
            },
            |frame| {
                for col in first_col..=last_col.min(hyo_core::MAX_COLS - 1) {
                    let x = HEADER_W + col as f32 * CELL_W - self.scroll.x;
                    let active = self.active.col == col;
                    if active {
                        frame.fill_rectangle(
                            Point::new(x, 0.0),
                            Size::new(CELL_W, HEADER_H),
                            HEADER_ACTIVE,
                        );
                    }
                    frame.fill_text(Text {
                        content: column_name(col),
                        position: Point::new(x + CELL_W / 2.0, HEADER_H / 2.0),
                        max_width: CELL_W,
                        color: if active { TEXT } else { HEADER_TEXT },
                        size: 11.0.into(),
                        align_x: TextAlignment::Center,
                        align_y: Vertical::Center,
                        ..Text::default()
                    });
                }
            },
        );

        // Row header strip.
        frame.fill_rectangle(
            Point::new(0.0, HEADER_H),
            Size::new(HEADER_W, grid_h),
            HEADER_BG,
        );
        frame.with_clip(
            Rectangle {
                x: 0.0,
                y: HEADER_H,
                width: HEADER_W,
                height: grid_h,
            },
            |frame| {
                for row in first_row..=last_row.min(hyo_core::MAX_ROWS - 1) {
                    let y = HEADER_H + row as f32 * CELL_H - self.scroll.y;
                    let active = self.active.row == row;
                    if active {
                        frame.fill_rectangle(
                            Point::new(0.0, y),
                            Size::new(HEADER_W, CELL_H),
                            HEADER_ACTIVE,
                        );
                    }
                    frame.fill_text(Text {
                        content: (row + 1).to_string(),
                        position: Point::new(HEADER_W - 8.0, y + CELL_H / 2.0),
                        max_width: HEADER_W,
                        color: if active { TEXT } else { HEADER_TEXT },
                        size: 11.0.into(),
                        align_x: TextAlignment::Right,
                        align_y: Vertical::Center,
                        ..Text::default()
                    });
                }
            },
        );

        // Frozen corner.
        frame.fill_rectangle(Point::ORIGIN, Size::new(HEADER_W, HEADER_H), HEADER_ACTIVE);
        frame.fill_text(Text {
            content: "fx".to_string(),
            position: Point::new(HEADER_W / 2.0, HEADER_H / 2.0),
            max_width: HEADER_W,
            color: TEXT,
            size: 11.0.into(),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
            ..Text::default()
        });

        draw_scrollbars(self, &mut frame, bounds, grid_w, grid_h);
        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        _state: &mut (),
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let canvas::Event::Mouse(event) = event else {
            return None;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let position = cursor.position_in(bounds)?;
                if let Some(addr) = self.header_cell_at(position, bounds) {
                    return Some(canvas::Action::publish(Message::SelectCell(addr)).and_capture());
                }
                if let Some(addr) = cell_at(position, bounds, self.scroll) {
                    return Some(canvas::Action::publish(Message::SelectCell(addr)).and_capture());
                }
                None
            }
            mouse::Event::WheelScrolled { delta } => {
                let (x, y) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => (x * CELL_W * 0.8, y * CELL_H * 3.0),
                    mouse::ScrollDelta::Pixels { x, y } => (*x, *y),
                };
                Some(canvas::Action::publish(Message::Scroll(Vector::new(-x, -y))).and_capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.position_in(bounds).is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl Grid<'_> {
    fn header_cell_at(&self, position: Point, bounds: Rectangle) -> Option<CellAddr> {
        if position.x < HEADER_W && position.y < HEADER_H {
            return Some(CellAddr::new(0, 0));
        }
        if position.y < HEADER_H && position.x > HEADER_W {
            let col = ((position.x - HEADER_W + self.scroll.x) / CELL_W).floor();
            if col >= 0.0 && col < hyo_core::MAX_COLS as f32 {
                return Some(CellAddr::new(self.active.row, col as u32));
            }
        }
        if position.x < HEADER_W && position.y > HEADER_H && position.y < bounds.height {
            let row = ((position.y - HEADER_H + self.scroll.y) / CELL_H).floor();
            if row >= 0.0 && row < hyo_core::MAX_ROWS as f32 {
                return Some(CellAddr::new(row as u32, self.active.col));
            }
        }
        None
    }
}

fn cell_at(position: Point, bounds: Rectangle, scroll: Vector) -> Option<CellAddr> {
    if position.x <= HEADER_W
        || position.y <= HEADER_H
        || position.x >= bounds.width - SCROLLBAR
        || position.y >= bounds.height - SCROLLBAR
    {
        return None;
    }
    let x = position.x - HEADER_W + scroll.x;
    let y = position.y - HEADER_H + scroll.y;
    let col = (x / CELL_W).floor();
    let row = (y / CELL_H).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    let addr = CellAddr::new(row as u32, col as u32);
    addr.in_bounds().then_some(addr)
}

fn cell_text(value: &Value, x: f32, y: f32) -> (String, Color, TextAlignment, Point) {
    let (mut content, color, align) = match value {
        Value::Number(_) => (value.display(), TEXT, TextAlignment::Right),
        Value::Text(_) => (value.display(), TEXT, TextAlignment::Left),
        Value::Bool(_) => (value.display(), ACCENT, TextAlignment::Center),
        Value::Error(_) => (value.display(), ERROR, TextAlignment::Left),
        Value::Empty => (String::new(), TEXT_MUTED, TextAlignment::Left),
    };
    if content.chars().count() > MAX_TEXT_CHARS {
        content = content.chars().take(MAX_TEXT_CHARS).collect::<String>() + "…";
    }
    let position = match align {
        TextAlignment::Right => Point::new(x + CELL_W - 8.0, y + CELL_H / 2.0),
        TextAlignment::Center => Point::new(x + CELL_W / 2.0, y + CELL_H / 2.0),
        _ => Point::new(x + 8.0, y + CELL_H / 2.0),
    };
    (content, color, align, position)
}

fn draw_scrollbars(
    grid: &Grid<'_>,
    frame: &mut Frame,
    bounds: Rectangle,
    grid_w: f32,
    grid_h: f32,
) {
    frame.fill_rectangle(
        Point::new(bounds.width - SCROLLBAR, HEADER_H),
        Size::new(SCROLLBAR, grid_h),
        SCROLLBAR_TRACK,
    );
    frame.fill_rectangle(
        Point::new(HEADER_W, bounds.height - SCROLLBAR),
        Size::new(grid_w, SCROLLBAR),
        SCROLLBAR_TRACK,
    );

    let max = max_scroll(grid.workbook, grid.sheet, grid_w, grid_h);

    if max.y > 0.0 {
        let content_h = max.y + grid_h;
        let thumb_h = (grid_h * grid_h / content_h).clamp(24.0, grid_h);
        let ratio = (grid.scroll.y / max.y).clamp(0.0, 1.0);
        let y = HEADER_H + (grid_h - thumb_h) * ratio;
        frame.fill_rectangle(
            Point::new(bounds.width - SCROLLBAR + 2.0, y),
            Size::new(SCROLLBAR - 4.0, thumb_h),
            SCROLLBAR_THUMB,
        );
    }

    if max.x > 0.0 {
        let content_w = max.x + grid_w;
        let thumb_w = (grid_w * grid_w / content_w).clamp(24.0, grid_w);
        let ratio = (grid.scroll.x / max.x).clamp(0.0, 1.0);
        let x = HEADER_W + (grid_w - thumb_w) * ratio;
        frame.fill_rectangle(
            Point::new(x, bounds.height - SCROLLBAR + 2.0),
            Size::new(thumb_w, SCROLLBAR - 4.0),
            SCROLLBAR_THUMB,
        );
    }
}

/// Maximum pixel offset in each axis, derived from the used range plus a
/// little runway so the grid does not imply a million empty rows.
pub fn max_scroll(workbook: &Workbook, sheet: usize, grid_w: f32, grid_h: f32) -> Vector {
    let bounds = workbook.sheet(sheet).and_then(|sheet| sheet.used_bounds());
    let used_cols = bounds
        .map(|b| b.end.col + 26)
        .unwrap_or(52)
        .min(hyo_core::MAX_COLS);
    let used_rows = bounds
        .map(|b| b.end.row + 200)
        .unwrap_or(200)
        .min(hyo_core::MAX_ROWS);
    Vector::new(
        (used_cols as f32 * CELL_W - grid_w).max(0.0),
        (used_rows as f32 * CELL_H - grid_h).max(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_hit_testing_accounts_for_scroll() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 900.0,
            height: 600.0,
        };
        let addr = cell_at(
            Point::new(HEADER_W + 5.0, HEADER_H + 5.0),
            bounds,
            Vector::new(0.0, 0.0),
        );
        assert_eq!(addr, Some(CellAddr::new(0, 0)));

        let addr = cell_at(
            Point::new(HEADER_W + 5.0, HEADER_H + 5.0),
            bounds,
            Vector::new(CELL_W * 2.0, CELL_H * 3.0),
        );
        assert_eq!(addr, Some(CellAddr::new(3, 2)));
    }
}
