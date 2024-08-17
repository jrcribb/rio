/*
    Crosswords -> Rio's grid manager

    |----------------------------------|
    |-$-bash:-echo-1-------------------|
    |-1--------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|

// Crosswords (mod.rs) was originally taken from https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/term/mod.rs
// which is licensed under Apache 2.0 license.
*/

pub mod attr;
pub mod grid;
pub mod pos;
pub mod square;
pub mod vi_mode;

use crate::ansi::graphics::GraphicCell;
use crate::ansi::graphics::Graphics;
use crate::ansi::graphics::TextureRef;
use crate::ansi::graphics::UpdateQueues;
use crate::ansi::graphics::MAX_GRAPHIC_DIMENSIONS;
use crate::ansi::sixel;
use crate::ansi::{
    mode::Mode as AnsiMode, ClearMode, CursorShape, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, TabulationClearMode,
};
use crate::clipboard::ClipboardType;
use crate::config::colors::{
    self,
    term::{List, TermColors},
    AnsiColor, ColorRgb,
};
use crate::crosswords::grid::{BidirectionalIterator, Dimensions, Grid, Scroll};
use crate::event::WindowId;
use crate::event::{EventListener, RioEvent};
use crate::performer::handler::Handler;
use crate::selection::{Selection, SelectionRange, SelectionType};
use attr::*;
use base64::{engine::general_purpose, Engine as _};
use bitflags::bitflags;
use copa::Params;
use grid::row::Row;
use log::{debug, info, warn};
use pos::{
    Boundary, CharsetIndex, Column, Cursor, CursorState, Direction, Line, Pos, Side,
};
use square::{Hyperlink, LineLength, Square};
use std::collections::HashSet;
use std::mem;
use std::ops::{Index, IndexMut, Range};
use std::option::Option;
use std::ptr;
use std::sync::Arc;
use sugarloaf::SugarGraphicData;
use unicode_width::UnicodeWidthChar;
use vi_mode::{ViModeCursor, ViMotion};

pub type NamedColor = colors::NamedColor;

pub const MIN_COLUMNS: usize = 2;
pub const MIN_LINES: usize = 1;
const BRACKET_PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// Max. number of graphics stored in a single cell.
const MAX_GRAPHICS_PER_CELL: usize = 20;

bitflags! {
    #[derive(Debug, Copy, Clone)]
     pub struct Mode: u32 {
        const NONE                             = 0;
        const SHOW_CURSOR                      = 0b0000_0000_0000_0000_0000_0001;
        const APP_CURSOR                       = 0b0000_0000_0000_0000_0000_0010;
        const APP_KEYPAD                       = 0b0000_0000_0000_0000_0000_0100;
        const MOUSE_REPORT_CLICK               = 0b0000_0000_0000_0000_0000_1000;
        const BRACKETED_PASTE                  = 0b0000_0000_0000_0000_0001_0000;
        const SGR_MOUSE                        = 0b0000_0000_0000_0000_0010_0000;
        const MOUSE_MOTION                     = 0b0000_0000_0000_0000_0100_0000;
        const LINE_WRAP                        = 0b0000_0000_0000_0000_1000_0000;
        const LINE_FEED_NEW_LINE               = 0b0000_0000_0000_0001_0000_0000;
        const ORIGIN                           = 0b0000_0000_0000_0010_0000_0000;
        const INSERT                           = 0b0000_0000_0000_0100_0000_0000;
        const FOCUS_IN_OUT                     = 0b0000_0000_0000_1000_0000_0000;
        const ALT_SCREEN                       = 0b0000_0000_0001_0000_0000_0000;
        const MOUSE_DRAG                       = 0b0000_0000_0010_0000_0000_0000;
        const MOUSE_MODE                       = 0b0000_0000_0010_0000_0100_1000;
        const UTF8_MOUSE                       = 0b0000_0000_0100_0000_0000_0000;
        const ALTERNATE_SCROLL                 = 0b0000_0000_1000_0000_0000_0000;
        const VI                               = 0b0000_0001_0000_0000_0000_0000;
        const URGENCY_HINTS                    = 0b0000_0010_0000_0000_0000_0000;
        const KEYBOARD_DISAMBIGUATE_ESC_CODES  = 0b0000_0100_0000_0000_0000_0000;
        const KEYBOARD_REPORT_EVENT_TYPES      = 0b0000_1000_0000_0000_0000_0000;
        const KEYBOARD_REPORT_ALTERNATE_KEYS   = 0b0001_0000_0000_0000_0000_0000;
        const KEYBOARD_REPORT_ALL_KEYS_AS_ESC  = 0b0010_0000_0000_0000_0000_0000;
        const KEYBOARD_REPORT_ASSOCIATED_TEXT  = 0b0100_0000_0000_0000_0000_0000;
        const KEYBOARD_PROTOCOL = Self::KEYBOARD_DISAMBIGUATE_ESC_CODES.bits()
                                | Self::KEYBOARD_REPORT_EVENT_TYPES.bits()
                                | Self::KEYBOARD_REPORT_ALTERNATE_KEYS.bits()
                                | Self::KEYBOARD_REPORT_ALL_KEYS_AS_ESC.bits()
                                | Self::KEYBOARD_REPORT_ASSOCIATED_TEXT.bits();
        const SIXEL_DISPLAY       = 0b1000_0000_0000_0000_0000;
        const SIXEL_PRIV_PALETTE  = 0b1000_0000_0000_0000_0001;
        const SIXEL_CURSOR_TO_THE_RIGHT  = 0b1000_0000_0000_0000_0010;
        const ANY                 = u32::MAX;
    }
}

impl Default for Mode {
    fn default() -> Mode {
        Mode::SHOW_CURSOR
            | Mode::LINE_WRAP
            | Mode::ALTERNATE_SCROLL
            | Mode::URGENCY_HINTS
            | Mode::SIXEL_PRIV_PALETTE
    }
}

impl From<KeyboardModes> for Mode {
    fn from(value: KeyboardModes) -> Self {
        let mut mode = Self::empty();
        mode.set(
            Mode::KEYBOARD_DISAMBIGUATE_ESC_CODES,
            value.contains(KeyboardModes::DISAMBIGUATE_ESC_CODES),
        );
        mode.set(
            Mode::KEYBOARD_REPORT_EVENT_TYPES,
            value.contains(KeyboardModes::REPORT_EVENT_TYPES),
        );
        mode.set(
            Mode::KEYBOARD_REPORT_ALTERNATE_KEYS,
            value.contains(KeyboardModes::REPORT_ALTERNATE_KEYS),
        );
        mode.set(
            Mode::KEYBOARD_REPORT_ALL_KEYS_AS_ESC,
            value.contains(KeyboardModes::REPORT_ALL_KEYS_AS_ESC),
        );
        mode.set(
            Mode::KEYBOARD_REPORT_ASSOCIATED_TEXT,
            value.contains(KeyboardModes::REPORT_ASSOCIATED_TEXT),
        );
        mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineDamageBounds {
    /// Damaged line number.
    pub line: usize,

    /// Leftmost damaged column.
    pub left: usize,

    /// Rightmost damaged column.
    pub right: usize,
}

impl LineDamageBounds {
    #[inline]
    pub fn undamaged(num_cols: usize, line: usize) -> Self {
        Self {
            line,
            left: num_cols,
            right: 0,
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn reset(&mut self, num_cols: usize) {
        *self = Self::undamaged(num_cols, self.line);
    }

    #[inline]
    pub fn expand(&mut self, left: usize, right: usize) {
        self.left = std::cmp::min(self.left, left);
        self.right = std::cmp::max(self.right, right);
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_damaged(&self) -> bool {
        self.left <= self.right
    }
}

#[derive(Debug, Clone)]
struct TermDamageState {
    /// Hint whether terminal should be damaged entirely regardless of the actual damage changes.
    is_fully_damaged: bool,

    /// Information about damage on terminal lines.
    lines: Vec<LineDamageBounds>,

    /// Old terminal cursor point.
    last_cursor: Pos,

    /// Last Vi cursor point.
    last_vi_cursor_point: Option<Pos>,
    // Old selection range.
    last_selection: Option<SelectionRange>,
}

impl TermDamageState {
    fn new(num_cols: usize, num_lines: usize) -> Self {
        let lines = (0..num_lines)
            .map(|line| LineDamageBounds::undamaged(num_cols, line))
            .collect();

        Self {
            is_fully_damaged: true,
            lines,
            last_cursor: Default::default(),
            last_vi_cursor_point: Default::default(),
            last_selection: Default::default(),
        }
    }

    #[inline]
    fn resize(&mut self, num_cols: usize, num_lines: usize) {
        // Reset point, so old cursor won't end up outside of the viewport.
        self.last_cursor = Default::default();
        self.last_vi_cursor_point = None;
        self.last_selection = None;
        self.is_fully_damaged = true;

        self.lines.clear();
        self.lines.reserve(num_lines);
        for line in 0..num_lines {
            self.lines.push(LineDamageBounds::undamaged(num_cols, line));
        }
    }

    /// Damage point inside of the viewport.
    #[inline]
    fn damage_point(&mut self, pos: Pos) {
        self.damage_line(pos.row.0 as usize, pos.col.0, pos.col.0);
    }

    /// Expand `line`'s damage to span at least `left` to `right` column.
    #[inline]
    fn damage_line(&mut self, line: usize, left: usize, right: usize) {
        self.lines[line].expand(left, right);
    }

    #[allow(dead_code)]
    fn damage_selection(
        &mut self,
        selection: SelectionRange,
        display_offset: usize,
        num_cols: usize,
    ) {
        let display_offset = display_offset as i32;
        let last_visible_line = self.lines.len() as i32 - 1;

        // Don't damage invisible selection.
        if selection.end.row.0 + display_offset < 0
            || selection.start.row.0.abs() < display_offset - last_visible_line
        {
            return;
        };

        let start = std::cmp::max(selection.start.row.0 + display_offset, 0);
        let end = (selection.end.row.0 + display_offset).clamp(0, last_visible_line);
        for line in start as usize..=end as usize {
            self.damage_line(line, 0, num_cols - 1);
        }
    }

    /// Reset information about terminal damage.
    fn reset(&mut self, num_cols: usize) {
        self.is_fully_damaged = false;
        self.lines.iter_mut().for_each(|line| line.reset(num_cols));
    }
}

#[derive(Debug, Clone)]
struct TabStops {
    tabs: Vec<bool>,
}

/// Default tab interval, corresponding to terminfo `it` value.
const INITIAL_TABSTOPS: usize = 8;

impl TabStops {
    #[inline]
    fn new(columns: usize) -> TabStops {
        TabStops {
            tabs: (0..columns).map(|i| i % INITIAL_TABSTOPS == 0).collect(),
        }
    }

    /// Remove all tabstops.
    #[inline]
    fn clear_all(&mut self) {
        unsafe {
            ptr::write_bytes(self.tabs.as_mut_ptr(), 0, self.tabs.len());
        }
    }

    /// Increase tabstop capacity.
    #[inline]
    fn resize(&mut self, columns: usize) {
        let mut index = self.tabs.len();
        self.tabs.resize_with(columns, || {
            let is_tabstop = index % INITIAL_TABSTOPS == 0;
            index += 1;
            is_tabstop
        });
    }
}

impl Index<Column> for TabStops {
    type Output = bool;

    fn index(&self, index: Column) -> &bool {
        &self.tabs[index.0]
    }
}

impl IndexMut<Column> for TabStops {
    fn index_mut(&mut self, index: Column) -> &mut bool {
        self.tabs.index_mut(index.0)
    }
}

/// Terminal version for escape sequence reports.
///
/// This returns the current terminal version as a unique number based on alacritty_terminal's
/// semver version. The different versions are padded to ensure that a higher semver version will
/// always report a higher version number.
fn version_number(mut version: &str) -> usize {
    if let Some(separator) = version.rfind('-') {
        version = &version[..separator];
    }

    let mut version_number = 0;

    let semver_versions = version.split('.');
    for (i, semver_version) in semver_versions.rev().enumerate() {
        let semver_number = semver_version.parse::<usize>().unwrap_or(0);
        version_number += usize::pow(100, i as u32) * semver_number;
    }

    version_number
}

// Max size of the window title stack.
const TITLE_STACK_MAX_DEPTH: usize = 4096;

// Max size of the keyboard modes.
const KEYBOARD_MODE_STACK_MAX_DEPTH: usize = 16384;

#[derive(Debug, Clone)]
pub struct Crosswords<U>
where
    U: EventListener,
{
    active_charset: CharsetIndex,
    mode: Mode,
    pub vi_mode_cursor: ViModeCursor,
    semantic_escape_chars: String,
    pub grid: Grid<Square>,
    inactive_grid: Grid<Square>,
    scroll_region: Range<Line>,
    tabs: TabStops,
    event_proxy: U,
    pub selection: Option<Selection>,
    #[allow(dead_code)]
    colors: List,
    pub title: String,
    damage: TermDamageState,
    graphics: Graphics,
    pub cursor_shape: CursorShape,
    pub default_cursor_shape: CursorShape,
    pub blinking_cursor: bool,
    pub window_id: WindowId,
    pub route_id: usize,
    title_stack: Vec<String>,
    hyperlink_re: regex::Regex,

    // The stack for the keyboard modes.
    keyboard_mode_stack: Vec<KeyboardModes>,

    // Currently inactive keyboard mode stack.
    inactive_keyboard_mode_stack: Vec<KeyboardModes>,
}

impl<U: EventListener> Crosswords<U> {
    pub fn new<D: Dimensions>(
        dimensions: D,
        cursor_shape: CursorShape,
        event_proxy: U,
        window_id: WindowId,
        route_id: usize,
    ) -> Crosswords<U> {
        let cols = dimensions.columns();
        let rows = dimensions.screen_lines();
        let grid = Grid::new(rows, cols, 10_000);
        let alt = Grid::new(rows, cols, 0);

        let scroll_region = Line(0)..Line(rows as i32);
        let semantic_escape_chars = String::from(",│`|:\"' ()[]{}<>\t");
        let term_colors = TermColors::default();
        let colors = List::from(&term_colors);
        // Regex used for the default URL hint.
        let url_regex: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)\
                         [^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`]+";

        Crosswords {
            vi_mode_cursor: ViModeCursor::new(grid.cursor.pos),
            semantic_escape_chars,
            selection: None,
            grid,
            inactive_grid: alt,
            active_charset: CharsetIndex::default(),
            scroll_region,
            event_proxy,
            colors,
            hyperlink_re: regex::Regex::new(url_regex).unwrap(),
            title: String::from(""),
            tabs: TabStops::new(cols),
            mode: Mode::SHOW_CURSOR
                | Mode::LINE_WRAP
                | Mode::ALTERNATE_SCROLL
                | Mode::URGENCY_HINTS,
            damage: TermDamageState::new(cols, rows),
            graphics: Graphics::new(&dimensions),
            default_cursor_shape: cursor_shape,
            cursor_shape,
            blinking_cursor: false,
            window_id,
            route_id,
            title_stack: Default::default(),
            keyboard_mode_stack: Default::default(),
            inactive_keyboard_mode_stack: Default::default(),
        }
    }

    pub fn mark_fully_damaged(&mut self) {
        self.damage.is_fully_damaged = true;
    }

    #[allow(dead_code)]
    pub fn reset_damage(&mut self) {
        self.damage.reset(self.grid.columns());
    }

    #[inline]
    pub fn display_offset(&self) -> usize {
        self.grid.display_offset()
    }

    #[inline]
    pub fn clear_saved_history(&mut self) {
        self.clear_screen(ClearMode::Saved);
    }

    #[inline]
    pub fn scroll_display(&mut self, scroll: Scroll) {
        let old_display_offset = self.grid.display_offset();
        self.event_proxy
            .send_event(RioEvent::MouseCursorDirty, self.window_id);
        self.grid.scroll_display(scroll);

        // Clamp vi mode cursor to the viewport.
        let viewport_start = -(self.grid.display_offset() as i32);
        let viewport_end = viewport_start + self.grid.bottommost_line().0;
        let vi_cursor_line = &mut self.vi_mode_cursor.pos.row.0;
        *vi_cursor_line =
            std::cmp::min(viewport_end, std::cmp::max(viewport_start, *vi_cursor_line));
        self.vi_mode_recompute_selection();

        // Damage everything if display offset changed.
        if old_display_offset != self.grid.display_offset() {
            self.mark_fully_damaged();

            self.event_proxy
                .send_event(RioEvent::Wakeup, self.window_id);
        }
    }

    #[inline]
    pub fn bottommost_line(&self) -> Line {
        self.grid.bottommost_line()
    }

    #[inline]
    pub fn colors(&self) -> List {
        self.colors
    }

    /// Get queues to update graphic data. If both queues are empty, it returns
    /// `None`.
    #[inline]
    pub fn graphics_take_queues(&mut self) -> Option<UpdateQueues> {
        self.graphics.take_queues()
    }

    #[inline]
    pub fn exit(&mut self)
    where
        U: EventListener,
    {
        self.event_proxy
            .send_event(RioEvent::CloseTerminal, self.window_id);
    }

    pub fn resize<S: Dimensions>(&mut self, size: S) {
        let old_cols = self.grid.columns();
        let old_lines = self.grid.screen_lines();
        let num_cols = size.columns();
        let num_lines = size.screen_lines();

        if old_cols == num_cols && old_lines == num_lines {
            info!("Crosswords::resize dimensions unchanged");
            return;
        }
        // Move vi mode cursor with the content.
        let history_size = self.history_size();
        let mut delta = num_lines as i32 - old_lines as i32;
        let min_delta =
            std::cmp::min(0, num_lines as i32 - self.grid.cursor.pos.row.0 - 1);

        delta = std::cmp::min(std::cmp::max(delta, min_delta), history_size as i32);
        self.vi_mode_cursor.pos.row += delta;

        let is_alt = self.mode.contains(Mode::ALT_SCREEN);
        self.grid.resize(!is_alt, num_lines, num_cols);
        self.inactive_grid.resize(is_alt, num_lines, num_cols);

        // Invalidate selection and tabs only when necessary.
        if old_cols != num_cols {
            self.selection = None;

            // Recreate tabs list.
            self.tabs.resize(num_cols);
        } else if let Some(selection) = self.selection.take() {
            let max_lines = std::cmp::max(num_lines, old_lines) as i32;
            let range = Line(0)..Line(max_lines);
            self.selection = selection.rotate(&self.grid, &range, -delta);
        }

        // Clamp vi cursor to viewport.
        let vi_pos = self.vi_mode_cursor;
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let viewport_bottom = viewport_top + self.bottommost_line();
        self.vi_mode_cursor.pos.row =
            std::cmp::max(std::cmp::min(vi_pos.pos.row, viewport_bottom), viewport_top);
        self.vi_mode_cursor.pos.col =
            std::cmp::min(vi_pos.pos.col, self.grid.last_column());

        // Reset scrolling region.
        self.scroll_region = Line(0)..Line(self.grid.screen_lines() as i32);

        // Resize damage information.
        self.damage.resize(num_cols, num_lines);

        // Update size information for graphics.
        self.graphics.resize(&size);
    }

    /// Toggle the vi mode.
    #[inline]
    pub fn toggle_vi_mode(&mut self)
    where
        U: EventListener,
    {
        self.mode ^= Mode::VI;

        if self.mode.contains(Mode::VI) {
            let display_offset = self.grid.display_offset() as i32;
            if self.grid.cursor.pos.row > self.grid.bottommost_line() - display_offset {
                // Move cursor to top-left if terminal cursor is not visible.
                let pos = Pos::new(Line(-display_offset), Column(0));
                self.vi_mode_cursor.pos = pos;
            } else {
                // Reset vi mode cursor position to match primary cursor.
                self.vi_mode_cursor.pos = self.grid.cursor.pos;
            }
        }

        // Update UI about cursor blinking state changes.
        self.event_proxy
            .send_event(RioEvent::CursorBlinkingChange, self.window_id);
    }

    /// Update the active selection to match the vi mode cursor position.
    #[inline]
    fn vi_mode_recompute_selection(&mut self) {
        // Require vi mode to be active.
        if !self.mode.contains(Mode::VI) {
            return;
        }

        // Update only if non-empty selection is present.
        if let Some(selection) = self.selection.as_mut().filter(|s| !s.is_empty()) {
            selection.update(self.vi_mode_cursor.pos, Side::Left);
            selection.include_all();
        }
    }

    #[inline]
    pub fn vi_motion(&mut self, motion: ViMotion)
    where
        U: EventListener,
    {
        // Require vi mode to be active.
        if !self.mode.contains(Mode::VI) {
            return;
        }

        // Move cursor.
        self.vi_mode_cursor = self.vi_mode_cursor.motion(self, motion);
        self.vi_mode_recompute_selection();
    }

    /// Scroll display to point if it is outside of viewport.
    #[inline]
    pub fn scroll_to_pos(&mut self, pos: Pos)
    where
        U: EventListener,
    {
        let display_offset = self.grid.display_offset() as i32;
        let screen_lines = self.grid.screen_lines() as i32;

        if pos.row < -display_offset {
            let lines = pos.row + display_offset;
            self.scroll_display(Scroll::Delta(-lines.0));
        } else if pos.row >= (screen_lines - display_offset) {
            let lines = pos.row + display_offset - screen_lines + 1i32;
            self.scroll_display(Scroll::Delta(-lines.0));
        }
    }

    /// Jump to the end of a wide cell.
    pub fn expand_wide(&self, mut pos: Pos, direction: Direction) -> Pos {
        let flags = self.grid[pos.row][pos.col].flags;

        match direction {
            Direction::Right
                if flags.contains(square::Flags::LEADING_WIDE_CHAR_SPACER) =>
            {
                pos.col = Column(1);
                pos.row += 1;
            }
            Direction::Right if flags.contains(square::Flags::WIDE_CHAR) => {
                pos.col = std::cmp::min(pos.col + 1, self.grid.last_column());
            }
            Direction::Left
                if flags.intersects(
                    square::Flags::WIDE_CHAR | square::Flags::WIDE_CHAR_SPACER,
                ) =>
            {
                if flags.contains(square::Flags::WIDE_CHAR_SPACER) {
                    pos.col -= 1;
                }

                let prev = pos.sub(&self.grid, Boundary::Grid, 1);
                if self.grid[prev]
                    .flags
                    .contains(square::Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    pos = prev;
                }
            }
            _ => (),
        }

        pos
    }

    #[inline]
    pub fn semantic_escape_chars(&self) -> &str {
        &self.semantic_escape_chars
    }

    #[inline]
    pub fn wrapline(&mut self) {
        if !self.mode.contains(Mode::LINE_WRAP) {
            return;
        }

        self.grid
            .cursor_cell()
            .flags
            .insert(square::Flags::WRAPLINE);

        if self.grid.cursor.pos.row + 1 >= self.scroll_region.end {
            self.linefeed();
        } else {
            self.damage_cursor();
            self.grid.cursor.pos.row += 1;
        }

        self.grid.cursor.pos.col = Column(0);
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    pub fn history_size(&self) -> usize {
        self.grid
            .total_lines()
            .saturating_sub(self.grid.screen_lines())
    }

    #[inline]
    fn damage_cursor(&mut self) {
        // The normal cursor coordinates are always in viewport.
        let point = Pos::new(Line(self.grid.cursor.pos.row.0), self.grid.cursor.pos.col);
        self.damage.damage_point(point);
    }

    #[inline]
    fn scroll_down_relative(&mut self, origin: Line, mut lines: usize) {
        debug!(
            "Scrolling down relative: origin={}, lines={}",
            origin, lines
        );

        lines = std::cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );
        lines = std::cmp::min(lines, (self.scroll_region.end - origin).0 as usize);

        let region = origin..self.scroll_region.end;

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(&self.grid, &region, -(lines as i32)));

        // Scroll vi mode cursor.
        let line = &mut self.vi_mode_cursor.pos.row;
        if region.start <= *line && region.end > *line {
            *line = std::cmp::min(*line + lines, region.end - 1);
        }

        // Scroll between origin and bottom
        self.grid.scroll_down(&region, lines);
        self.mark_fully_damaged();
    }

    #[inline]
    pub fn scroll_up_relative(&mut self, origin: Line, mut lines: usize) {
        debug!("Scrolling up: origin={origin}, lines={lines}");

        lines = std::cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );

        let region = origin..self.scroll_region.end;

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(&self.grid, &region, lines as i32));

        self.grid.scroll_up(&region, lines);

        // Scroll vi mode cursor.
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let top = if region.start == 0 {
            viewport_top
        } else {
            region.start
        };
        let line = &mut self.vi_mode_cursor.pos.row;
        if (top <= *line) && region.end > *line {
            *line = std::cmp::max(*line - lines, top);
        }
        self.mark_fully_damaged();
    }

    #[inline]
    pub fn bracket_search(&self, point: Pos) -> Option<Pos> {
        let start_char = self.grid[point].c;

        // Find the matching bracket we're looking for
        let (forward, end_char) = BRACKET_PAIRS.iter().find_map(|(open, close)| {
            if open == &start_char {
                Some((true, *close))
            } else if close == &start_char {
                Some((false, *open))
            } else {
                None
            }
        })?;

        let mut iter = self.grid.iter_from(point);

        // For every character match that equals the starting bracket, we
        // ignore one bracket of the opposite type.
        let mut skip_pairs = 0;

        loop {
            // Check the next cell
            let cell = if forward { iter.next() } else { iter.prev() };

            // Break if there are no more cells
            let cell = match cell {
                Some(cell) => cell,
                None => break,
            };

            // Check if the bracket matches
            if cell.c == end_char && skip_pairs == 0 {
                return Some(cell.pos);
            } else if cell.c == start_char {
                skip_pairs += 1;
            } else if cell.c == end_char {
                skip_pairs -= 1;
            }
        }

        None
    }

    #[inline]
    pub fn semantic_search_left(&self, mut point: Pos) -> Pos {
        // Limit the starting point to the last line in the history
        point.row = std::cmp::max(point.row, self.grid.topmost_line());

        let mut iter = self.grid.iter_from(point);
        let last_column = self.grid.columns() - 1;

        let wide = square::Flags::WIDE_CHAR
            | square::Flags::WIDE_CHAR_SPACER
            | square::Flags::LEADING_WIDE_CHAR_SPACER;
        while let Some(cell) = iter.prev() {
            if !cell.flags.intersects(wide) && self.semantic_escape_chars.contains(cell.c)
            {
                break;
            }

            if cell.pos.col == last_column
                && !cell.flags.contains(square::Flags::WRAPLINE)
            {
                break; // cut off if on new line or hit escape char
            }

            point = cell.pos;
        }

        point
    }

    #[inline]
    pub fn semantic_search_right(&self, mut point: Pos) -> Pos {
        // Limit the starting point to the last line in the history
        point.row = std::cmp::max(point.row, self.grid.topmost_line());

        let wide = square::Flags::WIDE_CHAR
            | square::Flags::WIDE_CHAR_SPACER
            | square::Flags::LEADING_WIDE_CHAR_SPACER;
        let last_column = self.grid.columns() - 1;

        for cell in self.grid.iter_from(point) {
            if !cell.flags.intersects(wide) && self.semantic_escape_chars.contains(cell.c)
            {
                break;
            }

            point = cell.pos;

            if point.col == last_column && !cell.flags.contains(square::Flags::WRAPLINE) {
                break; // cut off if on new line or hit escape char
            }
        }

        point
    }

    #[inline]
    pub fn search_nearest_hyperlink_from_pos(
        &mut self,
        pos: Pos,
    ) -> Option<SelectionRange> {
        // Limit the starting pos to the last line in the history
        let wide = square::Flags::WIDE_CHAR
            | square::Flags::WIDE_CHAR_SPACER
            | square::Flags::LEADING_WIDE_CHAR_SPACER;

        let last_column = self.grid.columns() - 1;
        if pos.col > last_column {
            return None;
        }

        let first_column = 0;
        let last_row = self.grid.screen_lines();
        let starting_square: &Square = &self.grid[pos];

        let is_existent_hyperlink = starting_square.hyperlink().is_some();
        if !is_existent_hyperlink && starting_square.c == ' ' {
            return None;
        }

        let mut content: std::collections::VecDeque<char> =
            std::collections::VecDeque::from([starting_square.c]);
        // TODO: Remove positions_to_update and fully rely on
        // selection_end and selection_start
        let mut positions_to_update: Vec<Pos> = vec![pos];
        let mut selection_start: Pos = pos;
        let mut selection_end: Pos = pos;

        // Next adjacents squares
        for square in self.grid.iter_from(pos) {
            if square.hyperlink().is_some() {
                content.push_back(square.c);
                selection_end = square.pos;
                positions_to_update.push(square.pos);
                continue;
            }

            if square.flags.intersects(wide) || square.c == ' ' {
                break;
            }

            content.push_back(square.c);
            selection_end = square.pos;
            positions_to_update.push(square.pos);

            if pos.col == last_column && pos.row == last_row {
                break; // cut off if on new line or hit escape char
            }
        }

        // Previous adjacents squares
        let mut iter = self.grid.iter_from(pos);
        while let Some(square) = iter.prev() {
            if square.hyperlink().is_some() {
                content.push_front(square.c);
                selection_start = square.pos;
                positions_to_update.push(square.pos);
                continue;
            }

            if square.flags.intersects(wide) || square.c == ' ' {
                break;
            }

            content.push_front(square.c);
            selection_start = square.pos;
            positions_to_update.push(square.pos);

            if square.pos.col == first_column && square.pos.row == 0 {
                break; // cut off if on new line or hit escape char
            }
        }

        if is_existent_hyperlink {
            let range = SelectionRange {
                start: selection_start,
                end: selection_end,
                is_block: false,
            };
            return Some(range);
        }

        if content.len() <= 4 {
            return None;
        }

        let value = content.iter().collect::<String>();
        if let Some(uri) = self.hyperlink_re.find(&value) {
            let uri = uri.as_str().to_string();
            let hyperlink = Some(Hyperlink::new(None, uri));

            for link_pos in positions_to_update.iter() {
                self.grid[link_pos.row][link_pos.col].set_hyperlink(hyperlink.to_owned());
            }

            let range = SelectionRange {
                start: selection_start,
                end: selection_end,
                is_block: false,
            };
            return Some(range);
        }

        None
    }

    #[inline(always)]
    pub fn write_at_cursor(&mut self, c: char) {
        let c = self.grid.cursor.charsets[self.active_charset].map(c);
        let fg = self.grid.cursor.template.fg;
        let bg = self.grid.cursor.template.bg;
        let flags = self.grid.cursor.template.flags;
        let extra = self.grid.cursor.template.extra.clone();

        let mut cursor_square = self.grid.cursor_square();
        if cursor_square
            .flags
            .intersects(square::Flags::WIDE_CHAR | square::Flags::WIDE_CHAR_SPACER)
        {
            // Remove wide char and spacer.
            let wide = cursor_square.flags.contains(square::Flags::WIDE_CHAR);
            let point = self.grid.cursor.pos;
            if wide && point.col < self.grid.last_column() {
                self.grid[point.row][point.col + 1]
                    .flags
                    .remove(square::Flags::WIDE_CHAR_SPACER);
            } else if point.col > 0 {
                self.grid[point.row][point.col - 1].clear_wide();
            }

            // Remove leading spacers.
            if point.col <= 1 && point.row != self.grid.topmost_line() {
                let column = self.grid.last_column();
                self.grid[point.row - 1i32][column]
                    .flags
                    .remove(square::Flags::LEADING_WIDE_CHAR_SPACER);
            }

            cursor_square = self.grid.cursor_cell();
        }

        cursor_square.c = c;
        cursor_square.fg = fg;
        cursor_square.bg = bg;
        cursor_square.flags = flags;
        cursor_square.extra = extra;
    }

    #[inline]
    pub fn visible_rows(&self) -> Vec<Row<Square>> {
        let mut start = self.scroll_region.start.0;
        let mut end = self.scroll_region.end.0;
        let mut visible_rows = Vec::with_capacity(self.grid.screen_lines());

        let scroll = self.display_offset() as i32;
        if scroll != 0 {
            start -= scroll;
            end -= scroll;
        }

        for row in start..end {
            visible_rows.push(self.grid[Line(row)].to_owned());
        }

        visible_rows
    }

    fn deccolm(&mut self)
    where
        U: EventListener,
    {
        // Setting 132 column font makes no sense, but run the other side effects.
        // Clear scrolling region.
        self.set_scrolling_region(1, None);

        // Clear grid.
        self.grid.reset_region(..);
        self.mark_fully_damaged();
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    #[inline]
    pub fn cursor(&self) -> CursorState {
        let mut content = self.cursor_shape;
        let vi_mode = self.mode.contains(Mode::VI);
        let scroll = self.display_offset() as i32;
        let mut pos = if vi_mode {
            let mut vi_cursor_pos = self.vi_mode_cursor.pos;
            if scroll > 0 {
                vi_cursor_pos.row += scroll;
            }
            vi_cursor_pos
        } else {
            if scroll != 0 {
                content = CursorShape::Hidden;
            }
            self.grid.cursor.pos
        };
        if self.grid[pos]
            .flags
            .contains(square::Flags::WIDE_CHAR_SPACER)
        {
            pos.col -= 1;
        }

        // If the cursor is hidden then set content as hidden
        if !vi_mode && !self.mode.contains(Mode::SHOW_CURSOR) {
            content = CursorShape::Hidden;
        }

        // If is not using app cursor then use default
        if content != CursorShape::Hidden && !self.mode.contains(Mode::ALT_SCREEN) {
            content = self.default_cursor_shape;
        }

        CursorState { pos, content }
    }

    pub fn swap_alt(&mut self) {
        if !self.mode.contains(Mode::ALT_SCREEN) {
            // Set alt screen cursor to the current primary screen cursor.
            self.inactive_grid.cursor = self.grid.cursor.clone();

            // Drop information about the primary screens saved cursor.
            self.grid.saved_cursor = self.grid.cursor.clone();

            // Reset alternate screen contents.
            self.inactive_grid.reset_region(..);
        }

        mem::swap(
            &mut self.keyboard_mode_stack,
            &mut self.inactive_keyboard_mode_stack,
        );
        self.set_keyboard_mode(
            self.keyboard_mode_stack
                .last()
                .copied()
                .unwrap_or(KeyboardModes::NO_MODE)
                .into(),
            KeyboardModesApplyBehavior::Replace,
        );

        mem::swap(&mut self.grid, &mut self.inactive_grid);
        self.mode ^= Mode::ALT_SCREEN;
        self.selection = None;
        self.mark_fully_damaged();
    }

    pub fn selection_to_string(&self) -> Option<String> {
        let selection_range = self.selection.as_ref().and_then(|s| s.to_range(self))?;
        let SelectionRange { start, end, .. } = selection_range;

        let mut res = String::new();

        match self.selection.as_ref() {
            Some(Selection {
                ty: SelectionType::Block,
                ..
            }) => {
                for line in (start.row.0..end.row.0).map(Line::from) {
                    res += self
                        .line_to_string(line, start.col..end.col, start.col.0 != 0)
                        .trim_end();
                    res += "\n";
                }

                res += self
                    .line_to_string(end.row, start.col..end.col, true)
                    .trim_end();
            }
            Some(Selection {
                ty: SelectionType::Lines,
                ..
            }) => {
                res = self.bounds_to_string(start, end) + "\n";
            }
            _ => {
                res = self.bounds_to_string(start, end);
            }
        }

        Some(res)
    }

    pub fn bounds_to_string(&self, start: Pos, end: Pos) -> String {
        let mut res = String::new();

        for line in (start.row.0..=end.row.0).map(Line::from) {
            let start_col = if line == start.row {
                start.col
            } else {
                Column(0)
            };
            let end_col = if line == end.row {
                end.col
            } else {
                self.grid.last_column()
            };

            res += &self.line_to_string(line, start_col..end_col, line == end.row);
        }

        res.strip_suffix('\n').map(str::to_owned).unwrap_or(res)
    }

    /// Convert a single line in the grid to a String.
    fn line_to_string(
        &self,
        line: Line,
        mut cols: Range<Column>,
        include_wrapped_wide: bool,
    ) -> String {
        let mut text = String::new();

        let grid_line = &self.grid[line];
        let line_length = std::cmp::min(grid_line.line_length(), cols.end + 1);

        // Include wide char when trailing spacer is selected.
        if grid_line[cols.start]
            .flags
            .contains(square::Flags::WIDE_CHAR_SPACER)
        {
            cols.start -= 1;
        }

        let mut tab_mode = false;
        for column in (cols.start.0..line_length.0).map(Column::from) {
            let cell = &grid_line[column];

            // Skip over cells until next tab-stop once a tab was found.
            if tab_mode {
                if self.tabs[column] || cell.c != ' ' {
                    tab_mode = false;
                } else {
                    continue;
                }
            }

            if cell.c == '\t' {
                tab_mode = true;
            }

            if !cell.flags.intersects(
                square::Flags::WIDE_CHAR_SPACER | square::Flags::LEADING_WIDE_CHAR_SPACER,
            ) {
                // Push cells primary character.
                text.push(cell.c);

                // Push zero-width characters.
                for c in cell.zerowidth().into_iter().flatten() {
                    text.push(*c);
                }
            }
        }

        if cols.end >= self.grid.columns() - 1
            && (line_length.0 == 0
                || !self.grid[line][line_length - 1]
                    .flags
                    .contains(square::Flags::WRAPLINE))
        {
            text.push('\n');
        }

        // If wide char is not part of the selection, but leading spacer is, include it.
        if line_length == self.grid.columns()
            && line_length.0 >= 2
            && grid_line[line_length - 1]
                .flags
                .contains(square::Flags::LEADING_WIDE_CHAR_SPACER)
            && include_wrapped_wide
        {
            text.push(self.grid[line - 1i32][Column(0)].c);
        }

        text
    }

    #[inline]
    fn set_keyboard_mode(&mut self, mode: Mode, apply: KeyboardModesApplyBehavior) {
        // println!("{:?}", mode);
        let active_mode = self.mode & Mode::KEYBOARD_PROTOCOL;
        self.mode &= !Mode::KEYBOARD_PROTOCOL;
        let new_mode = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => active_mode.union(mode),
            KeyboardModesApplyBehavior::Difference => active_mode.difference(mode),
        };
        info!("Setting keyboard mode to {new_mode:?}");
        self.mode |= new_mode;
    }

    /// Find the beginning of the current line across linewraps.
    pub fn row_search_left(&self, mut point: Pos) -> Pos {
        while point.row > self.grid.topmost_line()
            && self.grid[point.row - 1i32][self.grid.last_column()]
                .flags
                .contains(square::Flags::WRAPLINE)
        {
            point.row -= 1;
        }

        point.col = Column(0);

        point
    }

    /// Find the end of the current line across linewraps.
    pub fn row_search_right(&self, mut point: Pos) -> Pos {
        while point.row + 1 < self.grid.screen_lines()
            && self.grid[point.row][self.grid.last_column()]
                .flags
                .contains(square::Flags::WRAPLINE)
        {
            point.row += 1;
        }

        point.col = self.grid.last_column();

        point
    }
}

impl<U: EventListener> Handler for Crosswords<U> {
    #[inline]
    fn set_mode(&mut self, mode: AnsiMode) {
        match mode {
            AnsiMode::UrgencyHints => self.mode.insert(Mode::URGENCY_HINTS),
            AnsiMode::SwapScreenAndSetRestoreCursor => {
                if !self.mode.contains(Mode::ALT_SCREEN) {
                    self.swap_alt();
                }
            }
            AnsiMode::ShowCursor => self.mode.insert(Mode::SHOW_CURSOR),
            AnsiMode::CursorKeys => self.mode.insert(Mode::APP_CURSOR),
            // Mouse protocols are mutually exclusive.
            AnsiMode::ReportMouseClicks => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_REPORT_CLICK);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportSquareMouseMotion => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_DRAG);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportAllMouseMotion => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_MOTION);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportFocusInOut => self.mode.insert(Mode::FOCUS_IN_OUT),
            AnsiMode::BracketedPaste => self.mode.insert(Mode::BRACKETED_PASTE),
            // Mouse encodings are mutually exclusive.
            AnsiMode::SgrMouse => {
                self.mode.remove(Mode::UTF8_MOUSE);
                self.mode.insert(Mode::SGR_MOUSE);
            }
            AnsiMode::Utf8Mouse => {
                self.mode.remove(Mode::SGR_MOUSE);
                self.mode.insert(Mode::UTF8_MOUSE);
            }
            AnsiMode::AlternateScroll => self.mode.insert(Mode::ALTERNATE_SCROLL),
            AnsiMode::LineWrap => self.mode.insert(Mode::LINE_WRAP),
            AnsiMode::LineFeedNewLine => self.mode.insert(Mode::LINE_FEED_NEW_LINE),
            AnsiMode::Origin => self.mode.insert(Mode::ORIGIN),
            AnsiMode::Column => self.deccolm(),
            AnsiMode::Insert => self.mode.insert(Mode::INSERT),
            AnsiMode::BlinkingCursor => {
                self.blinking_cursor = true;
                self.event_proxy
                    .send_event(RioEvent::CursorBlinkingChange, self.window_id);
            }
            AnsiMode::SixelDisplay => self.mode.insert(Mode::SIXEL_DISPLAY),
            AnsiMode::SixelPrivateColorRegisters => {
                self.mode.insert(Mode::SIXEL_PRIV_PALETTE)
            }
            AnsiMode::SixelCursorToTheRight => {
                self.mode.insert(Mode::SIXEL_CURSOR_TO_THE_RIGHT);
            }
        }
    }

    #[inline]
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        debug!(
            "Requested write of escape sequence for color code {}: color[{}]",
            prefix, index
        );

        let terminator = terminator.to_owned();
        self.event_proxy.send_event(
            RioEvent::ColorRequest(
                index,
                Arc::new(move |color| {
                    format!(
                        "\x1b]{};rgb:{1:02x}{1:02x}/{2:02x}{2:02x}/{3:02x}{3:02x}{4}",
                        prefix, color.r, color.g, color.b, terminator
                    )
                }),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn unset_mode(&mut self, mode: AnsiMode) {
        match mode {
            AnsiMode::UrgencyHints => self.mode.remove(Mode::URGENCY_HINTS),
            AnsiMode::SwapScreenAndSetRestoreCursor => {
                if self.mode.contains(Mode::ALT_SCREEN) {
                    self.swap_alt();
                }
            }
            AnsiMode::ShowCursor => self.mode.remove(Mode::SHOW_CURSOR),
            AnsiMode::CursorKeys => self.mode.remove(Mode::APP_CURSOR),
            AnsiMode::ReportMouseClicks => {
                self.mode.remove(Mode::MOUSE_REPORT_CLICK);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportSquareMouseMotion => {
                self.mode.remove(Mode::MOUSE_DRAG);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportAllMouseMotion => {
                self.mode.remove(Mode::MOUSE_MOTION);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            AnsiMode::ReportFocusInOut => self.mode.remove(Mode::FOCUS_IN_OUT),
            AnsiMode::BracketedPaste => self.mode.remove(Mode::BRACKETED_PASTE),
            AnsiMode::SgrMouse => self.mode.remove(Mode::SGR_MOUSE),
            AnsiMode::Utf8Mouse => self.mode.remove(Mode::UTF8_MOUSE),
            AnsiMode::AlternateScroll => self.mode.remove(Mode::ALTERNATE_SCROLL),
            AnsiMode::LineWrap => self.mode.remove(Mode::LINE_WRAP),
            AnsiMode::LineFeedNewLine => self.mode.remove(Mode::LINE_FEED_NEW_LINE),
            AnsiMode::Origin => self.mode.remove(Mode::ORIGIN),
            AnsiMode::Column => self.deccolm(),
            AnsiMode::Insert => {
                self.mode.remove(Mode::INSERT);
                self.mark_fully_damaged();
            }
            AnsiMode::BlinkingCursor => {
                // TODO: Update it
                // self.blinking_cursor = false;
                // self.event_proxy
                //     .send_event(RioEvent::CursorBlinkingChange, self.window_id);
            }
            AnsiMode::SixelDisplay => self.mode.remove(Mode::SIXEL_DISPLAY),
            AnsiMode::SixelPrivateColorRegisters => {
                self.graphics.sixel_shared_palette = None;
                self.mode.remove(Mode::SIXEL_PRIV_PALETTE);
            }
            AnsiMode::SixelCursorToTheRight => {
                self.mode.remove(Mode::SIXEL_CURSOR_TO_THE_RIGHT)
            }
        }
    }

    #[inline]
    fn goto(&mut self, line: Line, col: Column) {
        let (y_offset, max_y) = if self.mode.contains(Mode::ORIGIN) {
            (self.scroll_region.start, self.scroll_region.end - 1)
        } else {
            (Line(0), self.grid.bottommost_line())
        };

        self.damage_cursor();
        self.grid.cursor.pos.row =
            std::cmp::max(std::cmp::min(line + y_offset, max_y), Line(0));
        self.grid.cursor.pos.col = std::cmp::min(col, self.grid.last_column());
        self.damage_cursor();
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn set_active_charset(&mut self, index: CharsetIndex) {
        self.active_charset = index;
    }

    #[inline]
    fn move_forward(&mut self, cols: Column) {
        let last_column =
            std::cmp::min(self.grid.cursor.pos.col + cols, self.grid.last_column());

        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage
            .damage_line(cursor_line, self.grid.cursor.pos.col.0, last_column.0);

        self.grid.cursor.pos.col = last_column;
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_backward(&mut self, cols: Column) {
        let column = self.grid.cursor.pos.col.saturating_sub(cols.0);

        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage
            .damage_line(cursor_line, column, self.grid.cursor.pos.col.0);

        self.grid.cursor.pos.col = Column(column);
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_backward_tabs(&mut self, count: u16) {
        self.damage_cursor();

        let old_col = self.grid.cursor.pos.col.0;
        for _ in 0..count {
            let mut col = self.grid.cursor.pos.col;
            for i in (0..(col.0)).rev() {
                if self.tabs[Column(i)] {
                    col = Column(i);
                    break;
                }
            }
            self.grid.cursor.pos.col = col;
        }

        let line = self.grid.cursor.pos.row.0 as usize;
        self.damage
            .damage_line(line, self.grid.cursor.pos.col.0, old_col);
    }

    #[inline]
    fn goto_line(&mut self, line: Line) {
        self.goto(line, self.grid.cursor.pos.col)
    }

    #[inline]
    fn goto_col(&mut self, col: Column) {
        self.goto(self.grid.cursor.pos.row, col)
    }

    #[inline]
    fn decaln(&mut self) {
        for line in (0..self.grid.screen_lines()).map(Line::from) {
            for column in 0..self.grid.columns() {
                let cell = &mut self.grid[line][Column(column)];
                *cell = Square::default();
                cell.c = 'E';
            }
        }

        self.mark_fully_damaged();
    }

    #[inline]
    fn move_up(&mut self, rows: usize) {
        self.goto(self.grid.cursor.pos.row - rows, self.grid.cursor.pos.col)
    }

    #[inline]
    fn move_down(&mut self, rows: usize) {
        self.goto(self.grid.cursor.pos.row + rows, self.grid.cursor.pos.col)
    }

    #[inline]
    fn move_down_and_cr(&mut self, rows: usize) {
        self.goto(self.grid.cursor.pos.row + rows, Column(0))
    }

    #[inline]
    fn move_up_and_cr(&mut self, lines: usize) {
        self.goto(self.grid.cursor.pos.row - lines, Column(0))
    }

    #[inline]
    fn scroll_up(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_up_relative(origin, lines);
    }

    #[inline]
    fn delete_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.pos.row;
        let lines = std::cmp::min(self.grid.screen_lines() - origin.0 as usize, lines);

        if lines > 0 && self.scroll_region.contains(&origin) {
            self.scroll_up_relative(origin, lines);
        }
    }

    #[inline]
    fn push_title(&mut self) {
        log::trace!("Pushing '{:?}' onto title stack", self.title);

        if self.title_stack.len() >= TITLE_STACK_MAX_DEPTH {
            let removed = self.title_stack.remove(0);
            log::trace!(
                "Removing '{:?}' from bottom of title stack that exceeds its maximum depth",
                removed
            );
        }

        self.title_stack.push(self.title.clone());
    }

    #[inline]
    fn pop_title(&mut self) {
        log::trace!("Attempting to pop title from stack...");

        if let Some(popped) = self.title_stack.pop() {
            log::trace!("Title '{:?}' popped from stack", popped);
            self.set_title(Some(popped));
        }
    }

    #[inline]
    fn erase_chars(&mut self, count: Column) {
        let cursor = &self.grid.cursor;

        let start = cursor.pos.col;
        let end = std::cmp::min(start + count, Column(self.grid.columns()));

        // Cleared cells have current background color set.
        let bg = self.grid.cursor.template.bg;
        let line = cursor.pos.row;
        self.damage.damage_line(line.0 as usize, start.0, end.0);
        let row = &mut self.grid[line];
        for cell in &mut row[start..end] {
            *cell = bg.into();
        }
    }

    #[inline]
    fn delete_chars(&mut self, count: usize) {
        let columns = self.grid.columns();
        let cursor = &self.grid.cursor;
        let bg = cursor.template.bg;

        // Ensure deleting within terminal bounds.
        let count = std::cmp::min(count, columns);

        let start = cursor.pos.col.0;
        let end = std::cmp::min(start + count, columns - 1);
        let num_cells = columns - end;

        let line = cursor.pos.row;
        self.damage
            .damage_line(line.0 as usize, 0, self.grid.columns() - 1);
        let row = &mut self.grid[line][..];

        for offset in 0..num_cells {
            row.swap(start + offset, end + offset);
        }

        // Clear last `count` cells in the row. If deleting 1 char, need to delete
        // 1 cell.
        let end = columns - count;
        for cell in &mut row[end..] {
            *cell = bg.into();
        }
    }

    #[inline]
    fn scroll_down(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_down_relative(origin, lines);
    }

    #[inline]
    fn insert_blank_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.pos.row;
        if self.scroll_region.contains(&origin) {
            self.scroll_down_relative(origin, lines);
        }
    }

    #[inline]
    fn insert_blank(&mut self, count: usize) {
        let cursor = &self.grid.cursor;
        let bg = cursor.template.bg;

        // Ensure inserting within terminal bounds
        let count = std::cmp::min(count, self.grid.columns() - cursor.pos.col.0);

        let source = cursor.pos.col;
        let destination = cursor.pos.col.0 + count;
        let num_cells = self.grid.columns() - destination;

        let line = cursor.pos.row;
        self.damage
            .damage_line(line.0 as usize, 0, self.grid.columns() - 1);

        let row = &mut self.grid[line][..];

        for offset in (0..num_cells).rev() {
            row.swap(destination + offset, source.0 + offset);
        }

        // Squares were just moved out toward the end of the line;
        // fill in between source and dest with blanks.
        for cell in &mut row[source.0..destination] {
            *cell = bg.into();
        }
    }

    #[inline]
    fn reverse_index(&mut self) {
        // If cursor is at the top.
        if self.grid.cursor.pos.row == self.scroll_region.start {
            self.scroll_down(1);
        } else {
            self.damage_cursor();
            self.grid.cursor.pos.row =
                std::cmp::max(self.grid.cursor.pos.row - 1, Line(0));
            self.damage_cursor();
        }
    }

    #[inline]
    fn reset_state(&mut self) {
        if self.mode.contains(Mode::ALT_SCREEN) {
            std::mem::swap(&mut self.grid, &mut self.inactive_grid);
        }
        self.active_charset = Default::default();
        self.cursor_shape = self.default_cursor_shape;
        self.grid.reset();
        self.inactive_grid.reset();
        self.scroll_region = Line(0)..Line(self.grid.screen_lines() as i32);
        self.tabs = TabStops::new(self.grid.columns());
        self.title_stack = Vec::new();
        self.keyboard_mode_stack = Vec::new();
        self.title = String::from("");
        self.selection = None;
        self.vi_mode_cursor = Default::default();
        self.keyboard_mode_stack = Default::default();
        self.inactive_keyboard_mode_stack = Default::default();

        // Preserve vi mode across resets.
        self.mode &= Mode::VI;
        self.mode.insert(Mode::default());

        self.event_proxy
            .send_event(RioEvent::CursorBlinkingChange, self.window_id);
        self.mark_fully_damaged();
    }

    #[inline]
    fn terminal_attribute(&mut self, attr: Attr) {
        let cursor = &mut self.grid.cursor;
        match attr {
            Attr::Foreground(color) => cursor.template.fg = color,
            Attr::Background(color) => cursor.template.bg = color,
            Attr::UnderlineColor(color) => cursor.template.set_underline_color(color),
            Attr::Reset => {
                cursor.template.fg = AnsiColor::Named(NamedColor::Foreground);
                cursor.template.bg = AnsiColor::Named(NamedColor::Background);
                cursor.template.flags = square::Flags::empty();
                cursor.template.set_underline_color(None);
            }
            Attr::Reverse => cursor.template.flags.insert(square::Flags::INVERSE),
            Attr::CancelReverse => cursor.template.flags.remove(square::Flags::INVERSE),
            Attr::Bold => cursor.template.flags.insert(square::Flags::BOLD),
            Attr::CancelBold => cursor.template.flags.remove(square::Flags::BOLD),
            Attr::Dim => cursor.template.flags.insert(square::Flags::DIM),
            Attr::CancelBoldDim => cursor
                .template
                .flags
                .remove(square::Flags::BOLD | square::Flags::DIM),
            Attr::Italic => cursor.template.flags.insert(square::Flags::ITALIC),
            Attr::CancelItalic => cursor.template.flags.remove(square::Flags::ITALIC),
            Attr::Underline => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(square::Flags::UNDERLINE);
            }
            Attr::DoubleUnderline => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES);
                cursor
                    .template
                    .flags
                    .insert(square::Flags::DOUBLE_UNDERLINE);
            }
            Attr::Undercurl => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES);
                cursor.template.flags.insert(square::Flags::UNDERCURL);
            }
            Attr::DottedUnderline => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES);
                cursor
                    .template
                    .flags
                    .insert(square::Flags::DOTTED_UNDERLINE);
            }
            Attr::DashedUnderline => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES);
                cursor
                    .template
                    .flags
                    .insert(square::Flags::DASHED_UNDERLINE);
            }
            Attr::CancelUnderline => {
                cursor.template.flags.remove(square::Flags::ALL_UNDERLINES)
            }
            Attr::Hidden => cursor.template.flags.insert(square::Flags::HIDDEN),
            Attr::CancelHidden => cursor.template.flags.remove(square::Flags::HIDDEN),
            Attr::Strike => cursor.template.flags.insert(square::Flags::STRIKEOUT),
            Attr::CancelStrike => cursor.template.flags.remove(square::Flags::STRIKEOUT),
            _ => {
                warn!("Term got unhandled attr: {:?}", attr);
            }
        }
    }

    fn set_title(&mut self, title: Option<String>) {
        self.title = title.unwrap_or_default();
    }

    #[inline]
    fn set_cursor_style(&mut self, style: Option<CursorShape>, _blinking: bool) {
        if let Some(cursor_shape) = style {
            self.cursor_shape = cursor_shape;
        } else {
            self.cursor_shape = self.default_cursor_shape;
        }

        // self.blinking_cursor = blinking;
        // self.event_proxy
        //     .send_event(RioEvent::CursorBlinkingChange, self.window_id);
    }

    #[inline]
    fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_shape = shape;
    }

    #[inline]
    fn set_keypad_application_mode(&mut self) {
        log::trace!("Setting keypad application mode");
        self.mode.insert(Mode::APP_KEYPAD);
    }

    #[inline]
    fn unset_keypad_application_mode(&mut self) {
        log::trace!("Unsetting keypad application mode");
        self.mode.remove(Mode::APP_KEYPAD);
    }

    /// Store data into clipboard.
    #[inline]
    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        if let Ok(bytes) = general_purpose::STANDARD.decode(base64) {
            if let Ok(text) = String::from_utf8(bytes) {
                self.event_proxy.send_event(
                    RioEvent::ClipboardStore(clipboard_type, text),
                    self.window_id,
                );
            }
        }
    }

    #[inline]
    fn configure_charset(
        &mut self,
        index: pos::CharsetIndex,
        charset: pos::StandardCharset,
    ) {
        log::trace!("Configuring charset {:?} as {:?}", index, charset);
        self.grid.cursor.charsets[index] = charset;
    }

    #[inline(never)]
    fn input(&mut self, c: char) {
        let width = match c.width() {
            Some(width) => width,
            None => return,
        };

        // Handle zero-width characters.
        if width == 0 {
            // // Get previous column.
            let mut column = self.grid.cursor.pos.col;
            if !self.grid.cursor.should_wrap {
                column.0 = column.saturating_sub(1);
            }

            // // Put zerowidth characters over first fullwidth character cell.
            let row = self.grid.cursor.pos.row;
            if self.grid[row][column]
                .flags
                .contains(square::Flags::WIDE_CHAR_SPACER)
            {
                column.0 = column.saturating_sub(1);
            }

            self.grid[row][column].push_zerowidth(c);
            return;
        }

        if self.grid.cursor.should_wrap {
            self.wrapline();
        }

        let columns = self.grid.columns();
        if self.mode.contains(Mode::INSERT) && self.grid.cursor.pos.col + width < columns
        {
            let line = self.grid.cursor.pos.row;
            let col = self.grid.cursor.pos.col;
            let row = &mut self.grid[line][..];

            for col in (col.0..(columns - width)).rev() {
                row.swap(col + width, col);
            }
        }

        if width == 1 {
            self.write_at_cursor(c);
        } else {
            if self.grid.cursor.pos.col + 1 >= columns {
                if self.mode.contains(Mode::LINE_WRAP) {
                    // Insert placeholder before wide char if glyph does not fit in this row.
                    self.grid
                        .cursor
                        .template
                        .flags
                        .insert(square::Flags::LEADING_WIDE_CHAR_SPACER);
                    self.write_at_cursor(' ');
                    self.grid
                        .cursor
                        .template
                        .flags
                        .remove(square::Flags::LEADING_WIDE_CHAR_SPACER);
                    self.wrapline();
                } else {
                    // Prevent out of bounds crash when linewrapping is disabled.
                    self.grid.cursor.should_wrap = true;
                    return;
                }
            }

            self.grid
                .cursor
                .template
                .flags
                .insert(square::Flags::WIDE_CHAR);
            self.write_at_cursor(c);
            self.grid
                .cursor
                .template
                .flags
                .remove(square::Flags::WIDE_CHAR);

            // Write spacer to cell following the wide glyph.
            self.grid.cursor.pos.col += 1;
            self.grid
                .cursor
                .template
                .flags
                .insert(square::Flags::WIDE_CHAR_SPACER);
            self.write_at_cursor(' ');
            self.grid
                .cursor
                .template
                .flags
                .remove(square::Flags::WIDE_CHAR_SPACER);
        }

        if self.grid.cursor.pos.col + 1 < columns {
            self.grid.cursor.pos.col += 1;
        } else {
            self.grid.cursor.should_wrap = true;
        }
    }

    #[inline]
    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => {
                log::trace!("Reporting primary device attributes");
                let text = String::from("\x1b[?6c");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(text), self.window_id);
            }
            Some('>') => {
                log::trace!("Reporting secondary device attributes");
                let version = version_number(env!("CARGO_PKG_VERSION"));
                let text = format!("\x1b[>0;{version};1c");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(text), self.window_id);
            }
            _ => debug!("Unsupported device attributes intermediate"),
        }
    }

    #[inline]
    fn report_keyboard_mode(&mut self) {
        let current_mode = self
            .keyboard_mode_stack
            .last()
            .unwrap_or(&KeyboardModes::NO_MODE)
            .bits();
        let text = format!("\x1b[?{current_mode}u");
        self.event_proxy
            .send_event(RioEvent::PtyWrite(text), self.window_id);
    }

    #[inline]
    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        if self.keyboard_mode_stack.len() >= KEYBOARD_MODE_STACK_MAX_DEPTH {
            let _removed = self.title_stack.remove(0);
        }

        self.keyboard_mode_stack.push(mode);
        self.set_keyboard_mode(mode.into(), KeyboardModesApplyBehavior::Replace);
    }

    #[inline]
    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        let new_len = self
            .keyboard_mode_stack
            .len()
            .saturating_sub(to_pop as usize);
        self.keyboard_mode_stack.truncate(new_len);

        // Reload active mode.
        let mode = self
            .keyboard_mode_stack
            .last()
            .copied()
            .unwrap_or(KeyboardModes::NO_MODE);
        self.set_keyboard_mode(mode.into(), KeyboardModesApplyBehavior::Replace);
    }

    #[inline]
    fn set_keyboard_mode(
        &mut self,
        mode: KeyboardModes,
        apply: KeyboardModesApplyBehavior,
    ) {
        self.set_keyboard_mode(mode.into(), apply);
    }

    #[inline]
    fn device_status(&mut self, arg: usize) {
        log::trace!("Reporting device status: {}", arg);
        match arg {
            5 => {
                let text = String::from("\x1b[0n");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(text), self.window_id);
            }
            6 => {
                let pos = self.grid.cursor.pos;
                let text = format!("\x1b[{};{}R", pos.row + 1, pos.col + 1);
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(text), self.window_id);
            }
            _ => debug!("unknown device status query: {}", arg),
        };
    }

    #[inline]
    fn newline(&mut self) {
        self.linefeed();

        if self.mode.contains(Mode::LINE_FEED_NEW_LINE) {
            self.carriage_return();
        }
    }

    #[inline]
    fn backspace(&mut self) {
        if self.grid.cursor.pos.col > Column(0) {
            let line = self.grid.cursor.pos.row.0 as usize;
            let column = self.grid.cursor.pos.col.0;
            self.grid.cursor.pos.col -= 1;
            self.grid.cursor.should_wrap = false;
            self.damage.damage_line(line, column - 1, column);
        }
    }

    #[inline]
    fn clear_screen(&mut self, mode: ClearMode) {
        let bg = self.grid.cursor.template.bg;

        let screen_lines = self.grid.screen_lines();

        match mode {
            ClearMode::Above => {
                let cursor = self.grid.cursor.pos;

                // If clearing more than one line.
                if cursor.row > 1 {
                    // Fully clear all lines before the current line.
                    self.grid.reset_region(..cursor.row);
                }

                // Clear up to the current column in the current line.
                let end = std::cmp::min(cursor.col + 1, Column(self.grid.columns()));
                for cell in &mut self.grid[cursor.row][..end] {
                    *cell = bg.into();
                }

                let range = Line(0)..=cursor.row;
                self.selection =
                    self.selection.take().filter(|s| !s.intersects_range(range));
            }
            ClearMode::Below => {
                let cursor = self.grid.cursor.pos;
                for cell in &mut self.grid[cursor.row][cursor.col..] {
                    *cell = bg.into();
                }

                if (cursor.row.0 as usize) < screen_lines - 1 {
                    self.grid.reset_region((cursor.row + 1)..);
                }

                let range = cursor.row..Line(screen_lines as i32);
                self.selection =
                    self.selection.take().filter(|s| !s.intersects_range(range));
            }
            ClearMode::All => {
                if self.mode.contains(Mode::ALT_SCREEN) {
                    self.grid.reset_region(..);
                } else {
                    let old_offset = self.grid.display_offset();

                    self.grid.clear_viewport();

                    // Compute number of lines scrolled by clearing the viewport.
                    let lines = self.grid.display_offset().saturating_sub(old_offset);

                    self.vi_mode_cursor.pos.row = (self.vi_mode_cursor.pos.row - lines)
                        .grid_clamp(&self.grid, Boundary::Grid);
                }

                self.selection = None;
            }
            ClearMode::Saved if self.history_size() > 0 => {
                self.grid.clear_history();

                self.vi_mode_cursor.pos.row = self
                    .vi_mode_cursor
                    .pos
                    .row
                    .grid_clamp(&self.grid, Boundary::Cursor);

                self.selection = self
                    .selection
                    .take()
                    .filter(|s| !s.intersects_range(..Line(0)));
            }
            // We have no history to clear.
            ClearMode::Saved => (),
        }

        self.mark_fully_damaged();
    }

    #[inline]
    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        match mode {
            TabulationClearMode::Current => {
                self.tabs[self.grid.cursor.pos.col] = false;
            }
            TabulationClearMode::All => {
                self.tabs.clear_all();
            }
        }
    }

    #[inline]
    fn linefeed(&mut self) {
        let next = self.grid.cursor.pos.row + 1;
        if next == self.scroll_region.end {
            self.scroll_up_relative(self.scroll_region.start, 1);
        } else if next < self.grid.screen_lines() {
            self.damage_cursor();
            self.grid.cursor.pos.row += 1;
            self.damage_cursor();
        }
    }

    #[inline]
    fn set_horizontal_tabstop(&mut self) {
        self.tabs[self.grid.cursor.pos.col] = true;
    }

    #[inline]
    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        self.grid.cursor.template.set_hyperlink(hyperlink);
    }

    /// Set the indexed color value.
    #[inline]
    fn set_color(&mut self, _index: usize, _color: ColorRgb) {
        // Damage terminal if the color changed and it's not the cursor.
        // if index != NamedColor::Cursor as usize && self.colors[index] != Some(color) {
        // self.mark_fully_damaged();
        // }

        // self.colors[index] = Some(color);
    }

    #[inline]
    fn reset_color(&mut self, _index: usize) {
        // Damage terminal if the color changed and it's not the cursor.
        // if index != NamedColor::Cursor as usize && self.colors[index].is_some() {
        // self.mark_fully_damaged();
        // }

        // self.colors[index] = None;
    }

    #[inline]
    fn bell(&mut self) {
        warn!("[unimplemented] Bell");
    }

    #[inline]
    fn substitute(&mut self) {
        warn!("[unimplemented] Substitute");
    }

    #[inline]
    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        let terminator = terminator.to_owned();

        self.event_proxy.send_event(
            RioEvent::ClipboardLoad(
                clipboard_type,
                Arc::new(move |text| {
                    let base64 = general_purpose::STANDARD.encode(text);
                    format!("\x1b]52;{};{}{}", clipboard as char, base64, terminator)
                }),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn put_tab(&mut self, mut count: u16) {
        // A tab after the last column is the same as a linebreak.
        if self.grid.cursor.should_wrap {
            self.wrapline();
            return;
        }

        while self.grid.cursor.pos.col < self.grid.columns() && count != 0 {
            count -= 1;

            let c = self.grid.cursor.charsets[self.active_charset].map('\t');
            let cell = self.grid.cursor_square();
            if cell.c == ' ' {
                cell.c = c;
            }

            loop {
                if (self.grid.cursor.pos.col + 1) == self.grid.columns() {
                    break;
                }

                self.grid.cursor.pos.col += 1;

                if self.tabs[self.grid.cursor.pos.col] {
                    break;
                }
            }
        }
    }

    #[inline]
    fn carriage_return(&mut self) {
        let new_col = 0;
        let row = self.grid.cursor.pos.row.0 as usize;
        self.damage
            .damage_line(row, new_col, self.grid.cursor.pos.col.0);
        self.grid.cursor.pos.col = Column(new_col);
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_forward_tabs(&mut self, count: u16) {
        log::trace!("[unimplemented] Moving forward {} tabs", count);
    }

    #[inline]
    fn save_cursor_position(&mut self) {
        self.grid.saved_cursor = self.grid.cursor.clone();
    }

    #[inline]
    fn restore_cursor_position(&mut self) {
        self.damage_cursor();
        self.grid.cursor = self.grid.saved_cursor.clone();
        self.damage_cursor();
    }

    #[inline]
    fn clear_line(&mut self, mode: LineClearMode) {
        let cursor = &self.grid.cursor;
        let bg = cursor.template.bg;
        let point = cursor.pos;

        let (left, right) = match mode {
            LineClearMode::Right if cursor.should_wrap => return,
            LineClearMode::Right => (point.col, Column(self.grid.columns())),
            LineClearMode::Left => (Column(0), point.col + 1),
            LineClearMode::All => (Column(0), Column(self.grid.columns())),
        };

        self.damage
            .damage_line(point.row.0 as usize, left.0, right.0 - 1);

        let row = &mut self.grid[point.row];
        for cell in &mut row[left..right] {
            *cell = bg.into();
        }

        let range = self.grid.cursor.pos.row..=self.grid.cursor.pos.row;
        self.selection = self.selection.take().filter(|s| !s.intersects_range(range));
    }

    #[inline]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // Fallback to the last line as default.
        let bottom = bottom.unwrap_or_else(|| self.grid.screen_lines());

        if top >= bottom {
            warn!("Invalid scrolling region: ({};{})", top, bottom);
            return;
        }

        // Bottom should be included in the range, but range end is not
        // usually included. One option would be to use an inclusive
        // range, but instead we just let the open range end be 1
        // higher.
        let start = Line(top as i32 - 1);
        let end = Line(bottom as i32);

        debug!("Setting scrolling region: ({};{})", start, end);

        let screen_lines = Line(self.grid.screen_lines() as i32);
        self.scroll_region.start = std::cmp::min(start, screen_lines);
        self.scroll_region.end = std::cmp::min(end, screen_lines);
        self.goto(Line(0), Column(0));
    }

    #[inline]
    fn text_area_size_pixels(&mut self) {
        debug!("text_area_size_pixels");
        self.event_proxy.send_event(
            RioEvent::TextAreaSizeRequest(Arc::new(move |window_size| {
                let height = window_size.height;
                let width = window_size.width;
                format!("\x1b[4;{height};{width}t")
            })),
            self.window_id,
        );
    }

    #[inline]
    fn text_area_size_chars(&mut self) {
        let text = format!(
            "\x1b[8;{};{}t",
            self.grid.screen_lines(),
            self.grid.columns()
        );
        debug!("text_area_size_chars {:?}", text);
        self.event_proxy
            .send_event(RioEvent::PtyWrite(text), self.window_id);
    }

    #[inline]
    fn graphics_attribute(&mut self, pi: u16, pa: u16) {
        // From Xterm documentation:
        //
        //   CSI ? Pi ; Pa ; Pv S
        //
        //   Pi = 1  -> item is number of color registers.
        //   Pi = 2  -> item is Sixel graphics geometry (in pixels).
        //   Pi = 3  -> item is ReGIS graphics geometry (in pixels).
        //
        //   Pa = 1  -> read attribute.
        //   Pa = 2  -> reset to default.
        //   Pa = 3  -> set to value in Pv.
        //   Pa = 4  -> read the maximum allowed value.
        //
        //   Pv is ignored by xterm except when setting (Pa == 3).
        //   Pv = n <- A single integer is used for color registers.
        //   Pv = width ; height <- Two integers for graphics geometry.
        //
        //   xterm replies with a control sequence of the same form:
        //
        //   CSI ? Pi ; Ps ; Pv S
        //
        //   where Ps is the status:
        //   Ps = 0  <- success.
        //   Ps = 1  <- error in Pi.
        //   Ps = 2  <- error in Pa.
        //   Ps = 3  <- failure.
        //
        //   On success, Pv represents the value read or set.

        fn generate_response(pi: u16, ps: u16, pv: &[usize]) -> String {
            use std::fmt::Write;
            let mut text = format!("\x1b[?{};{}", pi, ps);
            for item in pv {
                let _ = write!(&mut text, ";{}", item);
            }
            text.push('S');
            text
        }

        let (ps, pv) = match pi {
            1 => {
                match pa {
                    1 => (0, &[sixel::MAX_COLOR_REGISTERS][..]), // current value is always the
                    // maximum
                    2 => (3, &[][..]), // Report unsupported
                    3 => (3, &[][..]), // Report unsupported
                    4 => (0, &[sixel::MAX_COLOR_REGISTERS][..]),
                    _ => (2, &[][..]), // Report error in Pa
                }
            }

            2 => {
                match pa {
                    1 => {
                        self.event_proxy.send_event(
                            RioEvent::TextAreaSizeRequest(Arc::new(move |window_size| {
                                let width = window_size.width;
                                let height = window_size.height;
                                let graphic_dimensions = [
                                    std::cmp::min(
                                        width as usize,
                                        MAX_GRAPHIC_DIMENSIONS[0],
                                    ),
                                    std::cmp::min(
                                        height as usize,
                                        MAX_GRAPHIC_DIMENSIONS[1],
                                    ),
                                ];

                                let (ps, pv) = (0, &graphic_dimensions[..]);
                                generate_response(pi, ps, pv)
                            })),
                            self.window_id,
                        );
                        return;
                    }
                    2 => (3, &[][..]), // Report unsupported
                    3 => (3, &[][..]), // Report unsupported
                    4 => (0, &MAX_GRAPHIC_DIMENSIONS[..]),
                    _ => (2, &[][..]), // Report error in Pa
                }
            }

            3 => {
                (1, &[][..]) // Report error in Pi (ReGIS unknown)
            }

            _ => {
                (1, &[][..]) // Report error in Pi
            }
        };

        self.event_proxy.send_event(
            RioEvent::PtyWrite(generate_response(pi, ps, pv)),
            self.window_id,
        );
    }

    #[inline]
    fn start_sixel_graphic(&mut self, params: &Params) -> Option<Box<sixel::Parser>> {
        let palette = self.graphics.sixel_shared_palette.take();
        Some(Box::new(sixel::Parser::new(params, palette)))
    }

    #[inline]
    fn insert_graphic(
        &mut self,
        graphic: SugarGraphicData,
        palette: Option<Vec<ColorRgb>>,
    ) {
        let cell_width = self.graphics.cell_width as usize;
        let cell_height = self.graphics.cell_height as usize;

        // Store last palette if we receive a new one, and it is shared.
        if let Some(palette) = palette {
            if !self.mode.contains(Mode::SIXEL_PRIV_PALETTE) {
                self.graphics.sixel_shared_palette = Some(palette);
            }
        }

        if graphic.width > MAX_GRAPHIC_DIMENSIONS[0]
            || graphic.height > MAX_GRAPHIC_DIMENSIONS[1]
        {
            return;
        }

        let width = graphic.width as u16;
        let height = graphic.height as u16;

        if width == 0 || height == 0 {
            return;
        }

        let graphic_id = self.graphics.next_id();

        // If SIXEL_DISPLAY is disabled, the start of the graphic is the
        // cursor position, and the grid can be scrolled if the graphic is
        // larger than the screen. The cursor is moved to the next line
        // after the graphic.
        //
        // If it is disabled, the graphic starts at (0, 0), the grid is never
        // scrolled, and the cursor position is unmodified.

        let scrolling = !self.mode.contains(Mode::SIXEL_DISPLAY);

        let leftmost = if scrolling {
            self.grid.cursor.pos.col.0
        } else {
            0
        };

        // A very simple optimization is to detect is a new graphic is replacing
        // completely a previous one. This happens if the following conditions
        // are met:
        //
        // - Both graphics are attached to the same top-left cell.
        // - Both graphics have the same size.
        // - The new graphic does not contain transparent pixels.
        //
        // In this case, we will ignore cells with a reference to the replaced
        // graphic.

        let skip_textures = {
            if graphic.maybe_transparent() {
                HashSet::new()
            } else {
                let mut set = HashSet::new();

                let line = if scrolling {
                    self.grid.cursor.pos.row
                } else {
                    Line(0)
                };

                if let Some(old_graphics) = self.grid[line][Column(leftmost)].graphics() {
                    for graphic in old_graphics {
                        let tex = &*graphic.texture;
                        if tex.width == width
                            && tex.height == height
                            && tex.cell_height == cell_height
                        {
                            set.insert(tex.id);
                        }
                    }
                }

                set
            }
        };

        // Fill the cells under the graphic.
        //
        // The cell in the first column contains a reference to the
        // graphic, with the offset from the start. The rest of the
        // cells are not overwritten, allowing any text behind
        // transparent portions of the image to be visible.

        let texture = Arc::new(TextureRef {
            id: graphic_id,
            width,
            height,
            cell_height,
            texture_operations: Arc::downgrade(&self.graphics.texture_operations),
        });

        for (top, offset_y) in (0..).zip((0..height).step_by(cell_height)) {
            let line = if scrolling {
                self.grid.cursor.pos.row
            } else {
                // Check if the image is beyond the screen limit.
                if top >= self.grid.screen_lines() as i32 {
                    break;
                }

                Line(top)
            };

            // Store a reference to the graphic in the first column.
            let row_len = self.grid[line].len();
            for (left, offset_x) in (leftmost..).zip((0..width).step_by(cell_width)) {
                if left >= row_len {
                    break;
                }

                let texture_operations =
                    Arc::downgrade(&self.graphics.texture_operations);
                let graphic_cell = GraphicCell {
                    texture: texture.clone(),
                    offset_x,
                    offset_y,
                    texture_operations,
                };

                let mut cell = self.grid.cursor.template.clone();
                let cell_ref = &mut self.grid[line][Column(left)];

                // If the cell contains any graphics, and the region of the cell
                // is not fully filled by the new graphic, the old graphics are
                // kept in the cell.
                let graphics = match cell_ref.take_graphics() {
                    Some(mut old_graphics)
                        if old_graphics.iter().any(|graphic| {
                            !skip_textures.contains(&graphic.texture.id)
                        }) && !graphic.is_filled(
                            offset_x as usize,
                            offset_y as usize,
                            cell_width,
                            cell_height,
                        ) =>
                    {
                        // Ensure that we don't exceed the graphics limit per cell.
                        while old_graphics.len() >= MAX_GRAPHICS_PER_CELL {
                            drop(old_graphics.remove(0));
                        }

                        old_graphics.push(graphic_cell);
                        old_graphics
                    }

                    _ => smallvec::smallvec![graphic_cell],
                };

                cell.set_graphics(graphics);
                *cell_ref = cell;

                self.damage
                    .damage_point(Pos::new((line.0 as usize).into(), Column(left)));
            }

            if scrolling && offset_y < height.saturating_sub(cell_height as u16) {
                self.linefeed();
            }
        }

        if self.mode.contains(Mode::SIXEL_CURSOR_TO_THE_RIGHT) {
            let graphic_columns = (graphic.width + cell_width - 1) / cell_width;
            self.move_forward(Column(graphic_columns));
        } else if scrolling {
            self.linefeed();
            self.carriage_return();
        }

        // Add the graphic data to the pending queue.
        self.graphics.pending.push(SugarGraphicData {
            id: graphic_id,
            ..graphic
        });

        self.event_proxy
            .send_event(RioEvent::UpdateGraphicLibrary, self.window_id);
    }
}

pub struct CrosswordsSize {
    pub columns: usize,
    pub screen_lines: usize,
    pub width: u32,
    pub height: u32,
    pub square_width: u32,
    pub square_height: u32,
}

impl CrosswordsSize {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns,
            screen_lines,
            width: 0,
            height: 0,
            square_width: 0,
            square_height: 0,
        }
    }

    pub fn new_with_dimensions(
        columns: usize,
        screen_lines: usize,
        width: u32,
        height: u32,
        square_width: u32,
        square_height: u32,
    ) -> Self {
        Self {
            columns,
            screen_lines,
            width,
            height,
            square_width,
            square_height,
        }
    }
}

impl Dimensions for CrosswordsSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn square_width(&self) -> f32 {
        0.
    }

    fn square_height(&self) -> f32 {
        0.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crosswords::pos::{Column, Line, Pos, Side};
    use crate::crosswords::CrosswordsSize;
    use crate::event::VoidListener;

    #[test]
    fn scroll_up() {
        let size = CrosswordsSize::new(1, 10);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;
        let mut cw =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        for i in 0..10 {
            cw.grid[Line(i)][Column(0)].c = i as u8 as char;
        }

        cw.grid.scroll_up(&(Line(0)..Line(10)), 2);

        assert_eq!(cw.grid[Line(0)][Column(0)].c, '\u{2}');
        assert_eq!(cw.grid[Line(0)].occ, 1);
        assert_eq!(cw.grid[Line(1)][Column(0)].c, '\u{3}');
        assert_eq!(cw.grid[Line(1)].occ, 1);
        assert_eq!(cw.grid[Line(2)][Column(0)].c, '\u{4}');
        assert_eq!(cw.grid[Line(2)].occ, 1);
        assert_eq!(cw.grid[Line(3)][Column(0)].c, '\u{5}');
        assert_eq!(cw.grid[Line(3)].occ, 1);
        assert_eq!(cw.grid[Line(4)][Column(0)].c, '\u{6}');
        assert_eq!(cw.grid[Line(4)].occ, 1);
        assert_eq!(cw.grid[Line(5)][Column(0)].c, '\u{7}');
        assert_eq!(cw.grid[Line(5)].occ, 1);
        assert_eq!(cw.grid[Line(6)][Column(0)].c, '\u{8}');
        assert_eq!(cw.grid[Line(6)].occ, 1);
        assert_eq!(cw.grid[Line(7)][Column(0)].c, '\u{9}');
        assert_eq!(cw.grid[Line(7)].occ, 1);
        assert_eq!(cw.grid[Line(8)][Column(0)].c, ' '); // was 0.
        assert_eq!(cw.grid[Line(8)].occ, 0);
        assert_eq!(cw.grid[Line(9)][Column(0)].c, ' '); // was 1.
        assert_eq!(cw.grid[Line(9)].occ, 0);
    }

    #[test]
    fn test_linefeed() {
        let size = CrosswordsSize::new(1, 1);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut cw =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        assert_eq!(cw.grid.total_lines(), 1);

        cw.linefeed();
        assert_eq!(cw.grid.total_lines(), 2);
    }

    #[test]
    fn test_linefeed_moving_cursor() {
        let size = CrosswordsSize::new(1, 3);

        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut cw =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 0);

        cw.linefeed();
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 1);

        // Keep adding lines but keep cursor at max row
        for _ in 0..20 {
            cw.linefeed();
        }
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 2);
        assert_eq!(cw.grid.total_lines(), 22);
    }

    #[test]
    fn test_input() {
        let size = CrosswordsSize::new(5, 10);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut cw =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        for i in 0..4 {
            cw.grid[Line(0)][Column(i)].c = i as u8 as char;
        }
        cw.grid[Line(1)][Column(3)].c = 'b';

        assert_eq!(cw.grid[Line(0)][Column(0)].c, '\u{0}');
        assert_eq!(cw.grid[Line(0)][Column(1)].c, '\u{1}');
        assert_eq!(cw.grid[Line(0)][Column(2)].c, '\u{2}');
        assert_eq!(cw.grid[Line(0)][Column(3)].c, '\u{3}');
        assert_eq!(cw.grid[Line(0)][Column(4)].c, ' ');
        assert_eq!(cw.grid[Line(1)][Column(2)].c, ' ');
        assert_eq!(cw.grid[Line(1)][Column(3)].c, 'b');
        assert_eq!(cw.grid[Line(0)][Column(4)].c, ' ');
    }

    #[test]
    fn simple_selection_works() {
        let size = CrosswordsSize::new(5, 5);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        let grid = &mut term.grid;
        for i in 0..4 {
            if i == 1 {
                continue;
            }

            grid[Line(i)][Column(0)].c = '"';

            for j in 1..4 {
                grid[Line(i)][Column(j)].c = 'a';
            }

            grid[Line(i)][Column(4)].c = '"';
        }
        grid[Line(2)][Column(0)].c = ' ';
        grid[Line(2)][Column(4)].c = ' ';
        grid[Line(2)][Column(4)]
            .flags
            .insert(square::Flags::WRAPLINE);
        grid[Line(3)][Column(0)].c = ' ';

        // Multiple lines contain an empty line.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Pos {
                row: Line(0),
                col: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(2),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\"aaa\"\n\n aaa "))
        );

        // A wrapline.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Pos {
                row: Line(2),
                col: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from(" aaa  aaa\""))
        );
    }

    #[test]
    fn line_selection_works() {
        let size = CrosswordsSize::new(5, 1);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        let mut grid: Grid<Square> = Grid::new(1, 5, 0);
        for i in 0..5 {
            grid[Line(0)][Column(i)].c = 'a';
        }
        grid[Line(0)][Column(0)].c = '"';
        grid[Line(0)][Column(3)].c = '"';

        mem::swap(&mut term.grid, &mut grid);

        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Pos {
                row: Line(0),
                col: Column(3),
            },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("\"aa\"a\n")));
    }

    #[test]
    fn block_selection_works() {
        let size = CrosswordsSize::new(5, 5);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;

        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);
        let grid = &mut term.grid;
        for i in 1..4 {
            grid[Line(i)][Column(0)].c = '"';

            for j in 1..4 {
                grid[Line(i)][Column(j)].c = 'a';
            }

            grid[Line(i)][Column(4)].c = '"';
        }
        grid[Line(2)][Column(2)].c = ' ';
        grid[Line(2)][Column(4)]
            .flags
            .insert(square::Flags::WRAPLINE);
        grid[Line(3)][Column(4)].c = ' ';

        term.selection = Some(Selection::new(
            SelectionType::Block,
            Pos {
                row: Line(0),
                col: Column(3),
            },
            Side::Left,
        ));

        // The same column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(3),
                },
                Side::Right,
            );
        }
        assert_eq!(term.selection_to_string(), Some(String::from("\na\na\na")));

        // The first column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(0),
                },
                Side::Left,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\n\"aa\n\"a\n\"aa"))
        );

        // The last column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\na\"\na\"\na"))
        );
    }

    #[test]
    fn test_search_nearest_hyperlink_from_pos_on_single_line() {
        let size = CrosswordsSize::new(20, 3);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;
        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);

        let grid = &mut term.grid;
        for i in 0..19 {
            grid[Line(0)][Column(i)].c = ' ';
        }

        // First line does not contain any hyperlink (it is empty as well)
        let result = term
            .search_nearest_hyperlink_from_pos(Pos::new(pos::Line(0), pos::Column(2)));
        assert_eq!(result, None);

        let grid = &mut term.grid;
        for i in 0..19 {
            grid[Line(0)][Column(i)].c = 'a';
        }

        // First line does not contain any hyperlink (does not contain any link)
        let result = term
            .search_nearest_hyperlink_from_pos(Pos::new(pos::Line(0), pos::Column(0)));
        assert_eq!(result, None);

        // Cleanup line
        let grid = &mut term.grid;
        for i in 0..19 {
            grid[Line(0)][Column(i)].c = ' ';
        }

        let grid = &mut term.grid;
        let link: [char; 14] = [
            'h', 't', 't', 'p', 's', ':', '/', '/', 'r', 'i', 'o', '.', 'i', 'o',
        ];
        for (i, val) in link.iter().enumerate() {
            grid[Line(0)][Column(i)].c = *val;
        }

        assert_eq!(term.grid[Line(0)][Column(0)].c, 'h');
        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_none());
        assert_eq!(term.grid[Line(0)][Column(1)].c, 't');
        assert!(term.grid[Line(0)][Column(1)].hyperlink().is_none());
        assert_eq!(term.grid[Line(0)][Column(2)].c, 't');
        assert!(term.grid[Line(0)][Column(2)].hyperlink().is_none());
        assert_eq!(term.grid[Line(0)][Column(3)].c, 'p');
        assert!(term.grid[Line(0)][Column(3)].hyperlink().is_none());
        assert_eq!(term.grid[Line(0)][Column(12)].c, 'i');
        assert!(term.grid[Line(0)][Column(12)].hyperlink().is_none());
        assert_eq!(term.grid[Line(0)][Column(13)].c, 'o');
        assert!(term.grid[Line(0)][Column(13)].hyperlink().is_none());

        // First line does not a hyperlink from (0 to 13) position
        let result = term
            .search_nearest_hyperlink_from_pos(Pos::new(pos::Line(0), pos::Column(14)));
        assert_eq!(result, None);

        // From 'h'
        let result = term
            .search_nearest_hyperlink_from_pos(Pos::new(pos::Line(0), pos::Column(0)));
        assert_eq!(
            result,
            Some(SelectionRange {
                start: Pos {
                    row: Line(0),
                    col: Column(0)
                },
                end: Pos {
                    row: Line(0),
                    col: Column(13)
                },
                is_block: false
            })
        );

        assert_eq!(term.grid[Line(0)][Column(0)].c, 'h');
        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(0)][Column(0)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert_eq!(term.grid[Line(0)][Column(1)].c, 't');
        assert!(term.grid[Line(0)][Column(1)].hyperlink().is_some());
        assert_eq!(term.grid[Line(0)][Column(2)].c, 't');
        assert!(term.grid[Line(0)][Column(2)].hyperlink().is_some());
        assert_eq!(term.grid[Line(0)][Column(3)].c, 'p');
        assert!(term.grid[Line(0)][Column(3)].hyperlink().is_some());
        assert_eq!(term.grid[Line(0)][Column(12)].c, 'i');
        assert!(term.grid[Line(0)][Column(12)].hyperlink().is_some());
        assert_eq!(term.grid[Line(0)][Column(13)].c, 'o');
        assert!(term.grid[Line(0)][Column(13)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(0)][Column(13)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert_eq!(term.grid[Line(0)][Column(14)].c, ' ');
        assert!(term.grid[Line(0)][Column(14)].hyperlink().is_none());

        // From 'r' (this case should hit square hyperlink info)
        let result = term
            .search_nearest_hyperlink_from_pos(Pos::new(pos::Line(0), pos::Column(8)));
        assert_eq!(
            result,
            Some(SelectionRange {
                start: Pos {
                    row: Line(0),
                    col: Column(0)
                },
                end: Pos {
                    row: Line(0),
                    col: Column(13)
                },
                is_block: false
            })
        );

        assert_eq!(term.grid[Line(0)][Column(0)].c, 'h');
        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(0)][Column(0)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert_eq!(term.grid[Line(0)][Column(13)].c, 'o');
        assert!(term.grid[Line(0)][Column(13)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(0)][Column(13)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert_eq!(term.grid[Line(0)][Column(14)].c, ' ');
        assert!(term.grid[Line(0)][Column(14)].hyperlink().is_none());
    }

    #[test]
    fn test_search_nearest_hyperlink_from_pos_on_multiple_lines() {
        let size = CrosswordsSize::new(4, 4);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;
        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);

        let grid = &mut term.grid;
        grid[Line(0)][Column(0)].c = 'h';
        grid[Line(0)][Column(1)].c = 't';
        grid[Line(0)][Column(2)].c = 't';
        grid[Line(0)][Column(3)].c = 'p';
        grid[Line(1)][Column(0)].c = 's';
        grid[Line(1)][Column(1)].c = ':';
        grid[Line(1)][Column(2)].c = '/';
        grid[Line(1)][Column(3)].c = '/';
        grid[Line(2)][Column(0)].c = 'r';
        grid[Line(2)][Column(1)].c = 'i';
        grid[Line(2)][Column(2)].c = 'o';
        grid[Line(2)][Column(3)].c = '.';
        grid[Line(3)][Column(0)].c = 'i';
        grid[Line(3)][Column(1)].c = 'o';

        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_none());
        assert!(term.grid[Line(1)][Column(0)].hyperlink().is_none());
        assert!(term.grid[Line(2)][Column(0)].hyperlink().is_none());
        assert!(term.grid[Line(3)][Column(0)].hyperlink().is_none());

        // From ' ' after '.io'
        let result = term.search_nearest_hyperlink_from_pos(Pos {
            row: pos::Line(3),
            col: pos::Column(2),
        });
        assert_eq!(result, None);

        // From 'r'
        let result = term.search_nearest_hyperlink_from_pos(Pos {
            row: pos::Line(2),
            col: pos::Column(1),
        });
        assert_eq!(
            result,
            Some(SelectionRange {
                start: Pos {
                    row: Line(0),
                    col: Column(0)
                },
                end: Pos {
                    row: Line(3),
                    col: Column(1)
                },
                is_block: false
            })
        );

        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(0)][Column(0)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_some());
        assert!(term.grid[Line(0)][Column(1)].hyperlink().is_some());
        assert!(term.grid[Line(0)][Column(2)].hyperlink().is_some());
        assert!(term.grid[Line(0)][Column(3)].hyperlink().is_some());
        assert!(term.grid[Line(1)][Column(0)].hyperlink().is_some());
        assert!(term.grid[Line(1)][Column(1)].hyperlink().is_some());
        assert!(term.grid[Line(1)][Column(2)].hyperlink().is_some());
        assert!(term.grid[Line(1)][Column(3)].hyperlink().is_some());
        assert!(term.grid[Line(2)][Column(0)].hyperlink().is_some());
        assert!(term.grid[Line(2)][Column(1)].hyperlink().is_some());
        assert!(term.grid[Line(2)][Column(2)].hyperlink().is_some());
        assert!(term.grid[Line(2)][Column(3)].hyperlink().is_some());
        assert!(term.grid[Line(3)][Column(0)].hyperlink().is_some());
        assert_eq!(
            term.grid[Line(3)][Column(1)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );
        assert!(term.grid[Line(3)][Column(2)].hyperlink().is_none());
    }

    #[test]
    fn test_search_nearest_hyperlink_from_pos_on_existent_hyperlink() {
        let size = CrosswordsSize::new(4, 4);
        #[cfg(not(use_wa))]
        let window_id = crate::event::WindowId::from(0);
        #[cfg(use_wa)]
        let window_id = 0;
        let mut term =
            Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0);

        let grid = &mut term.grid;
        let hyperlink = Hyperlink::new(None, "https://rio.io");
        grid[Line(0)][Column(2)].c = 'r';
        grid[Line(0)][Column(2)].set_hyperlink(Some(hyperlink.clone()));
        grid[Line(0)][Column(3)].c = ' ';
        grid[Line(0)][Column(3)].set_hyperlink(Some(hyperlink.clone()));
        grid[Line(1)][Column(0)].c = '2';
        grid[Line(1)][Column(0)].set_hyperlink(Some(hyperlink.clone()));
        grid[Line(1)][Column(1)].c = ' ';
        grid[Line(1)][Column(2)].c = 'i';
        grid[Line(1)][Column(2)].set_hyperlink(Some(hyperlink.clone()));
        grid[Line(1)][Column(3)].c = 'o';
        grid[Line(1)][Column(3)].set_hyperlink(Some(hyperlink.clone()));

        // "  r "
        // "2 io"
        // "    "
        // "    "

        // Hyperlink that should be highlighted is "r 2"

        assert!(term.grid[Line(0)][Column(0)].hyperlink().is_none());
        assert!(term.grid[Line(1)][Column(0)].hyperlink().is_some());
        assert!(term.grid[Line(2)][Column(0)].hyperlink().is_none());

        let result = term.search_nearest_hyperlink_from_pos(Pos {
            row: pos::Line(0),
            col: pos::Column(0),
        });
        assert_eq!(result, None);

        let result = term.search_nearest_hyperlink_from_pos(Pos {
            row: pos::Line(0),
            col: pos::Column(3),
        });
        assert_eq!(
            result,
            Some(SelectionRange {
                start: Pos {
                    row: Line(0),
                    col: Column(2)
                },
                end: Pos {
                    row: Line(1),
                    col: Column(0)
                },
                is_block: false
            })
        );

        assert_eq!(
            term.grid[Line(0)][Column(2)].hyperlink().unwrap().uri(),
            "https://rio.io"
        );

        // Then we "promote" col 1/ row 1 to hyperlink and connecting with " io"
        term.grid[Line(1)][Column(1)].set_hyperlink(Some(hyperlink.clone()));
        let result = term.search_nearest_hyperlink_from_pos(Pos {
            row: pos::Line(0),
            col: pos::Column(3),
        });
        assert_eq!(
            result,
            Some(SelectionRange {
                start: Pos {
                    row: Line(0),
                    col: Column(2)
                },
                end: Pos {
                    row: Line(1),
                    col: Column(3)
                },
                is_block: false
            })
        );
    }

    #[test]
    fn parse_cargo_version() {
        assert_eq!(version_number("0.0.1-nightly"), 1);
        assert_eq!(version_number("0.1.2-nightly"), 1_02);
        assert_eq!(version_number("1.2.3-nightly"), 1_02_03);
        assert_eq!(version_number("999.99.99"), 9_99_99_99);
    }
}
