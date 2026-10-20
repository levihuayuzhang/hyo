//! Application state and Elm-style update/view logic.

use crate::demo;
use crate::grid::{self, CELL_H, CELL_W, Grid, HEADER_H, HEADER_W};
use hyo_core::{CellAddr, RecalcReport, Value, Workbook};
use hyo_gpu::GpuBackend;
use hyo_io::{load_xlsx, save_xlsx};
use iced::keyboard::key::Named;
use iced::widget::{Row, button, canvas, column, container, row, space, text};
use iced::{Alignment, Element, Fill, Font, Size, Subscription, Vector};
use std::path::{Path, PathBuf};

const ACCENT: iced::Color = iced::Color::from_rgb(0.180, 0.620, 0.880);
const MUTED: iced::Color = iced::Color::from_rgb(0.530, 0.590, 0.670);
const DANGER: iced::Color = iced::Color::from_rgb(1.0, 0.400, 0.400);
const CHROME_HEIGHT: f32 = 270.0;

#[derive(Debug, Clone)]
pub enum Message {
    SelectCell(CellAddr),
    Scroll(Vector),
    Key(iced::keyboard::Event),
    Resized(Size),
    New,
    Open,
    Save,
    SaveAs,
    Recalc,
    AddSheet,
    SelectSheet(usize),
}

pub struct HyoApp {
    workbook: Workbook,
    backend: GpuBackend,
    report: Option<RecalcReport>,
    active_sheet: usize,
    active_cell: CellAddr,
    formula_buffer: String,
    editing: bool,
    dirty: bool,
    path: Option<PathBuf>,
    status: String,
    scroll: Vector,
    viewport: Size,
}

impl HyoApp {
    pub fn new() -> Self {
        let backend = GpuBackend::auto();
        let mut status = "Demo workbook — type to edit, Ctrl+S to save".to_owned();
        let mut path = None;

        let workbook = if let Some(argument) = std::env::args().nth(1) {
            let candidate = PathBuf::from(&argument);
            match load_xlsx(&candidate) {
                Ok(workbook) => {
                    path = Some(candidate);
                    status = format!("Opened {}", display_path(path.as_deref()));
                    workbook
                }
                Err(error) => {
                    status = format!("Could not open {argument}: {error}");
                    demo::workbook()
                }
            }
        } else {
            demo::workbook()
        };

        let mut app = Self {
            workbook,
            backend,
            report: None,
            active_sheet: 0,
            active_cell: CellAddr::new(0, 0),
            formula_buffer: String::new(),
            editing: false,
            dirty: false,
            path,
            status,
            scroll: Vector::new(0.0, 0.0),
            viewport: Size::new(1280.0, 820.0),
        };
        app.recalculate();
        app.sync_formula_buffer();
        app
    }

    fn recalculate(&mut self) {
        let report = self.workbook.recalc(&self.backend);
        self.report = Some(report);
    }

    fn current_cell_input(&self) -> String {
        self.workbook
            .sheet(self.active_sheet)
            .and_then(|sheet| sheet.cell(self.active_cell))
            .map(|cell| cell.input.clone())
            .unwrap_or_default()
    }

    fn current_value(&self) -> Value {
        self.workbook
            .sheet(self.active_sheet)
            .map(|sheet| sheet.value(self.active_cell))
            .unwrap_or(Value::Empty)
    }

    fn sync_formula_buffer(&mut self) {
        self.formula_buffer = self.current_cell_input();
    }

    fn select_cell(&mut self, addr: CellAddr) {
        if self.editing && !self.commit_edit(false) {
            return;
        }
        self.active_cell = CellAddr::new(
            addr.row.min(hyo_core::MAX_ROWS - 1),
            addr.col.min(hyo_core::MAX_COLS - 1),
        );
        self.editing = false;
        self.sync_formula_buffer();
        self.ensure_visible();
        self.status = format!("Selected {}", self.active_cell);
    }

    fn move_selection(&mut self, d_row: i64, d_col: i64) {
        if self.editing && !self.commit_edit(false) {
            return;
        }
        let row =
            (self.active_cell.row as i64 + d_row).clamp(0, (hyo_core::MAX_ROWS - 1) as i64) as u32;
        let col =
            (self.active_cell.col as i64 + d_col).clamp(0, (hyo_core::MAX_COLS - 1) as i64) as u32;
        self.select_cell(CellAddr::new(row, col));
    }

    fn begin_edit(&mut self, initial: Option<String>) {
        if self.editing {
            return;
        }
        self.formula_buffer = initial.unwrap_or_else(|| self.current_cell_input());
        self.editing = true;
        self.status = "Editing — Enter commits, Esc cancels".to_owned();
    }

    fn cancel_edit(&mut self) {
        self.editing = false;
        self.sync_formula_buffer();
        self.status = "Edit cancelled".to_owned();
    }

    fn commit_edit(&mut self, move_down: bool) -> bool {
        if !self.editing {
            return true;
        }
        let input = self.formula_buffer.clone();
        if let Err(error) = self
            .workbook
            .set_input(self.active_sheet, self.active_cell, &input)
        {
            self.status = format!("Formula error: {error}");
            return false;
        }
        self.editing = false;
        self.dirty = true;
        self.recalculate();
        self.sync_formula_buffer();
        let addr = self.active_cell;
        self.status = format!("Committed {addr}");
        if move_down {
            self.move_selection(1, 0);
            self.status = format!("Committed {addr}");
        }
        true
    }

    fn clear_active(&mut self) {
        if let Some(sheet) = self.workbook.sheet_mut(self.active_sheet) {
            sheet.clear(self.active_cell);
        }
        self.dirty = true;
        self.recalculate();
        self.sync_formula_buffer();
        self.status = format!("Cleared {}", self.active_cell);
    }

    fn grid_size(&self) -> (f32, f32) {
        let width = (self.viewport.width - HEADER_W - 12.0).max(CELL_W);
        let height = (self.viewport.height - CHROME_HEIGHT - HEADER_H - 12.0).max(CELL_H);
        (width, height)
    }

    fn ensure_visible(&mut self) {
        let (grid_w, grid_h) = self.grid_size();
        let max = grid::max_scroll(&self.workbook, self.active_sheet, grid_w, grid_h);
        let x = self.active_cell.col as f32 * CELL_W;
        let y = self.active_cell.row as f32 * CELL_H;
        if x < self.scroll.x {
            self.scroll.x = x;
        } else if x + CELL_W > self.scroll.x + grid_w {
            self.scroll.x = x + CELL_W - grid_w;
        }
        if y < self.scroll.y {
            self.scroll.y = y;
        } else if y + CELL_H > self.scroll.y + grid_h {
            self.scroll.y = y + CELL_H - grid_h;
        }
        self.scroll.x = self.scroll.x.clamp(0.0, max.x);
        self.scroll.y = self.scroll.y.clamp(0.0, max.y);
    }

    fn scroll_by(&mut self, delta: Vector) {
        let (grid_w, grid_h) = self.grid_size();
        let max = grid::max_scroll(&self.workbook, self.active_sheet, grid_w, grid_h);
        self.scroll.x = (self.scroll.x + delta.x).clamp(0.0, max.x);
        self.scroll.y = (self.scroll.y + delta.y).clamp(0.0, max.y);
    }

    fn new_workbook(&mut self) {
        self.workbook = Workbook::new();
        self.path = None;
        self.dirty = false;
        self.active_sheet = 0;
        self.active_cell = CellAddr::new(0, 0);
        self.editing = false;
        self.scroll = Vector::new(0.0, 0.0);
        self.recalculate();
        self.sync_formula_buffer();
        self.status = "New workbook".to_owned();
    }

    fn open(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Excel workbook", &["xlsx"])
            .pick_file()
        else {
            self.status = "Open cancelled".to_owned();
            return;
        };
        match load_xlsx(&path) {
            Ok(workbook) => {
                self.workbook = workbook;
                self.path = Some(path);
                self.dirty = false;
                self.active_sheet = 0;
                self.active_cell = CellAddr::new(0, 0);
                self.editing = false;
                self.scroll = Vector::new(0.0, 0.0);
                self.recalculate();
                self.sync_formula_buffer();
                self.status = format!("Opened {}", display_path(self.path.as_deref()));
            }
            Err(error) => {
                self.status = format!("Open failed: {error}");
            }
        }
    }

    fn save(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        if let Some(path) = self.path.clone() {
            self.save_to(&path);
        } else {
            self.save_as();
        }
    }

    fn save_as(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Excel workbook", &["xlsx"])
            .set_file_name("hyo-sheet.xlsx")
            .save_file()
        else {
            self.status = "Save cancelled".to_owned();
            return;
        };
        if path.extension().is_none() {
            path.set_extension("xlsx");
        }
        self.save_to(&path);
    }

    fn save_to(&mut self, path: &Path) {
        match save_xlsx(&self.workbook, path) {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", display_path(Some(path)));
            }
            Err(error) => {
                self.status = format!("Save failed: {error}");
            }
        }
    }

    fn add_sheet(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        let mut index = 1;
        let name = loop {
            let candidate = format!("Sheet{index}");
            if self.workbook.sheet_index(&candidate).is_none() {
                break candidate;
            }
            index += 1;
        };
        match self.workbook.add_sheet(name) {
            Ok(sheet) => {
                self.workbook.set_active_sheet(sheet);
                self.active_sheet = sheet;
                self.active_cell = CellAddr::new(0, 0);
                self.scroll = Vector::new(0.0, 0.0);
                self.editing = false;
                self.sync_formula_buffer();
                self.status = format!("Added {}", self.workbook.sheet(sheet).unwrap().name());
                self.dirty = true;
            }
            Err(error) => self.status = error,
        }
    }

    fn select_sheet(&mut self, index: usize) {
        if !self.commit_edit(false) {
            return;
        }
        if self.workbook.set_active_sheet(index) {
            self.active_sheet = index;
            self.active_cell = CellAddr::new(0, 0);
            self.scroll = Vector::new(0.0, 0.0);
            self.editing = false;
            self.sync_formula_buffer();
            self.status = format!(
                "Sheet {}",
                self.workbook
                    .sheet(index)
                    .map(|sheet| sheet.name())
                    .unwrap_or("?")
            );
        }
    }

    fn handle_key(&mut self, event: iced::keyboard::Event) {
        let iced::keyboard::Event::KeyPressed {
            key,
            modifiers,
            text,
            ..
        } = event
        else {
            return;
        };

        if modifiers.command() {
            if let iced::keyboard::Key::Character(character) = key.as_ref() {
                match character.to_ascii_lowercase().as_str() {
                    "s" if modifiers.shift() => self.save_as(),
                    "s" => self.save(),
                    "o" => self.open(),
                    "n" => self.new_workbook(),
                    "r" => {
                        self.recalculate();
                        self.status = "Recalculated".to_owned();
                    }
                    _ => {}
                }
            }
            return;
        }

        let typed = text
            .as_ref()
            .map(|value| value.as_str())
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
            .map(str::to_owned);

        if self.editing {
            match &key {
                iced::keyboard::Key::Named(Named::Enter) => {
                    self.commit_edit(true);
                }
                iced::keyboard::Key::Named(Named::Tab) => {
                    if self.commit_edit(false) {
                        self.move_selection(0, 1);
                    }
                }
                iced::keyboard::Key::Named(Named::Escape) => self.cancel_edit(),
                iced::keyboard::Key::Named(Named::Backspace) => {
                    self.formula_buffer.pop();
                }
                iced::keyboard::Key::Named(Named::ArrowUp) => {
                    if self.commit_edit(false) {
                        self.move_selection(-1, 0);
                    }
                }
                iced::keyboard::Key::Named(Named::ArrowDown) => {
                    if self.commit_edit(false) {
                        self.move_selection(1, 0);
                    }
                }
                iced::keyboard::Key::Named(Named::ArrowLeft) => {
                    if self.commit_edit(false) {
                        self.move_selection(0, -1);
                    }
                }
                iced::keyboard::Key::Named(Named::ArrowRight) => {
                    if self.commit_edit(false) {
                        self.move_selection(0, 1);
                    }
                }
                _ => {
                    if let Some(typed) = typed {
                        self.formula_buffer.push_str(&typed);
                    }
                }
            }
            return;
        }

        match &key {
            iced::keyboard::Key::Named(Named::Enter) => self.move_selection(1, 0),
            iced::keyboard::Key::Named(Named::Tab) => self.move_selection(0, 1),
            iced::keyboard::Key::Named(Named::F2) => self.begin_edit(None),
            iced::keyboard::Key::Named(Named::Escape) => {
                self.status = format!("Selected {}", self.active_cell);
            }
            iced::keyboard::Key::Named(Named::ArrowUp) => self.move_selection(-1, 0),
            iced::keyboard::Key::Named(Named::ArrowDown) => self.move_selection(1, 0),
            iced::keyboard::Key::Named(Named::ArrowLeft) => self.move_selection(0, -1),
            iced::keyboard::Key::Named(Named::ArrowRight) => self.move_selection(0, 1),
            iced::keyboard::Key::Named(Named::PageUp) => self.move_selection(-20, 0),
            iced::keyboard::Key::Named(Named::PageDown) => self.move_selection(20, 0),
            iced::keyboard::Key::Named(Named::Home) => {
                self.move_selection(0, -(self.active_cell.col as i64))
            }
            iced::keyboard::Key::Named(Named::End) => {
                let last = self
                    .workbook
                    .sheet(self.active_sheet)
                    .and_then(|sheet| sheet.used_bounds())
                    .map(|bounds| bounds.end.col)
                    .unwrap_or(0);
                self.select_cell(CellAddr::new(self.active_cell.row, last));
            }
            iced::keyboard::Key::Named(Named::Delete)
            | iced::keyboard::Key::Named(Named::Backspace) => self.clear_active(),
            iced::keyboard::Key::Named(Named::Space) if typed.is_some() => {
                self.begin_edit(typed);
            }
            _ => {
                if let Some(typed) = typed {
                    self.begin_edit(Some(typed));
                }
            }
        }
    }

    fn report_text(&self) -> String {
        match &self.report {
            Some(report) => format!(
                "{:.2} ms · {} formulas · {} cycles · {} GPU calls",
                report.duration.as_secs_f64() * 1000.0,
                report.formula_count,
                report.cycle_count,
                report.stats.gpu_calls
            ),
            None => "not calculated".to_owned(),
        }
    }
}

pub fn update(state: &mut HyoApp, message: Message) {
    match message {
        Message::SelectCell(addr) => state.select_cell(addr),
        Message::Scroll(delta) => state.scroll_by(delta),
        Message::Key(event) => state.handle_key(event),
        Message::Resized(size) => {
            state.viewport = size;
            state.ensure_visible();
        }
        Message::New => state.new_workbook(),
        Message::Open => state.open(),
        Message::Save => state.save(),
        Message::SaveAs => state.save_as(),
        Message::Recalc => {
            if state.commit_edit(false) {
                state.recalculate();
                state.status = "Recalculated".to_owned();
            }
        }
        Message::AddSheet => state.add_sheet(),
        Message::SelectSheet(index) => state.select_sheet(index),
    }
}

pub fn view(state: &HyoApp) -> Element<'_, Message> {
    let file_label = display_path(state.path.as_deref());
    let backend_label = if state.backend.gpu_available() {
        format!("GPU · {}", state.backend.device_name())
    } else {
        "CPU reference".to_owned()
    };

    let toolbar = row![
        text("HYO").size(18).font(Font::MONOSPACE).color(ACCENT),
        text("SHEET").size(18).font(Font::MONOSPACE),
        text(file_label).size(12).color(MUTED),
        text(if state.dirty { "●" } else { "" })
            .size(12)
            .color(ACCENT),
        space::horizontal(),
        text("wgpu · Vulkan").size(10).color(MUTED),
        text(backend_label).size(11).color(MUTED),
        button("New").on_press(Message::New).style(button::text),
        button("Open").on_press(Message::Open).style(button::text),
        button("Save")
            .on_press(Message::Save)
            .style(button::primary),
        button("Save As")
            .on_press(Message::SaveAs)
            .style(button::text),
        button("Recalc")
            .on_press(Message::Recalc)
            .style(button::text),
    ]
    .spacing(8)
    .padding([8, 12])
    .align_y(Alignment::Center);

    let formula_text = if state.editing {
        format!("{}▏", state.formula_buffer)
    } else if state.formula_buffer.is_empty() {
        " ".to_owned()
    } else {
        state.formula_buffer.clone()
    };
    let formula_bar = row![
        container(text("fx").size(13).color(MUTED))
            .width(26)
            .center_x(26),
        container(
            text(state.active_cell.to_a1())
                .size(12)
                .font(Font::MONOSPACE)
        )
        .width(72)
        .center_x(72)
        .padding([8, 0])
        .style(container::bordered_box),
        container(text(formula_text).size(13).font(Font::MONOSPACE))
            .width(Fill)
            .padding([7, 10])
            .style(container::bordered_box),
    ]
    .spacing(6)
    .padding([4, 12])
    .align_y(Alignment::Center);

    let grid = canvas(Grid {
        workbook: &state.workbook,
        sheet: state.active_sheet,
        active: state.active_cell,
        scroll: state.scroll,
    })
    .width(Fill)
    .height((state.viewport.height - CHROME_HEIGHT).max(220.0));

    let mut tabs: Vec<Element<'_, Message>> = Vec::new();
    for (index, sheet) in state.workbook.sheets().enumerate() {
        let active = index == state.active_sheet;
        let mut tab = button(text(sheet.name()).size(12))
            .on_press(Message::SelectSheet(index))
            .padding([5, 10]);
        tab = if active {
            tab.style(button::primary)
        } else {
            tab.style(button::text)
        };
        tabs.push(tab.into());
    }
    tabs.push(
        button("+")
            .on_press(Message::AddSheet)
            .style(button::text)
            .padding([5, 10])
            .into(),
    );
    let tabs = Row::with_children(tabs)
        .spacing(2)
        .padding([4, 8])
        .align_y(Alignment::Center);

    let value = state.current_value();
    let selection = format!(
        "{} · {} · {}",
        state.active_cell.to_a1(),
        value.type_name(),
        if value.is_empty() {
            "—".to_owned()
        } else {
            value.display()
        }
    );
    let status = row![
        text(state.backend.status_label()).size(10).color(MUTED),
        text(state.report_text()).size(10).color(MUTED),
        space::horizontal(),
        text(selection).size(10).color(MUTED),
        text(&state.status).size(10).color(
            if state.status.contains("error") || state.status.contains("failed") {
                DANGER
            } else {
                ACCENT
            }
        ),
    ]
    .spacing(14)
    .padding([5, 10])
    .align_y(Alignment::Center);

    column![toolbar, formula_bar, grid, tabs, status]
        .width(Fill)
        .height(Fill)
        .into()
}

pub fn subscription(_state: &HyoApp) -> Subscription<Message> {
    Subscription::batch([
        iced::keyboard::listen().map(Message::Key),
        iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
    ])
}

pub fn title(state: &HyoApp) -> String {
    let file = display_path(state.path.as_deref());
    let marker = if state.dirty { " *" } else { "" };
    format!("Hyo Sheet — {file}{marker}")
}

fn display_path(path: Option<&Path>) -> String {
    path.and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .to_owned()
}
