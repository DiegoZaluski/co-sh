#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::missing_panics_doc
)]
use super::{CursorShape, CursorVisibility, Position};
use crate::term_screen::cell::color::ColorAttribute;
use crate::term_screen::cell::{AttributeChange, CellAttributes, unicode_column_width};
use finl_unicode::grapheme_clusters::Graphemes;
use serde::{Deserialize, Serialize};

use std::string::String;
use std::vec;
use std::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum LineAttribute {
    DoubleHeightTopHalfLine,
    DoubleHeightBottomHalfLine,
    DoubleWidthLine,
    SingleWidthLine,
}

/// `Change` describes an update operation to be applied to a `Surface`.
/// Changes to the active attributes (color, style), moving the cursor
/// and outputting text are examples of some of the values.
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum Change {
    /// Change a single attribute
    Attribute(AttributeChange),
    /// Change all possible attributes to the given set of values
    AllAttributes(CellAttributes),
    /// Add printable text.
    /// Control characters are rendered inert by transforming them
    /// to space.  CR and LF characters are interpreted by moving
    /// the cursor position.  CR moves the cursor to the start of
    /// the line and LF moves the cursor down to the next line.
    /// You typically want to use both together when sending in
    /// a line break.
    Text(String),
    /// Clear the screen to the specified color.
    /// Implicitly clears all attributes prior to clearing the screen.
    /// Moves the cursor to the home position (top left).
    ClearScreen(ColorAttribute),
    /// Clear from the current cursor X position to the rightmost
    /// edge of the screen.  The background color is set to the
    /// provided color.  The cursor position remains unchanged.
    ClearToEndOfLine(ColorAttribute),
    /// Clear from the current cursor X position to the rightmost
    /// edge of the screen on the current line.  Clear all of the
    /// lines below the current cursor Y position.  The background
    /// color is set ot the provided color.  The cursor position
    /// remains unchanged.
    ClearToEndOfScreen(ColorAttribute),
    /// Move the cursor to the specified `Position`.
    CursorPosition { x: Position, y: Position },
    /// Change the cursor color.
    CursorColor(ColorAttribute),
    /// Change the cursor shape
    CursorShape(CursorShape),
    /// Change the cursor visibility
    CursorVisibility(CursorVisibility),
    /// Scroll the `region_size` lines starting at `first_row` upwards
    /// by `scroll_count` lines.  The `scroll_count` lines at the top of
    /// the region are overwritten.  The `scroll_count` lines at the
    /// bottom of the region will become blank.
    ///
    /// After a region is scrolled, the cursor position is undefined,
    /// and the terminal's scroll region is set to the range specified.
    /// To restore scrolling behaviour to the full terminal window, an
    /// additional `Change::ScrollRegionUp { first_row: 0, region_size:
    /// height, scroll_count: 0 }`, where `height` is the height of the
    /// terminal, should be emitted.
    ScrollRegionUp {
        first_row: usize,
        region_size: usize,
        scroll_count: usize,
    },
    /// Scroll the `region_size` lines starting at `first_row` downwards
    /// by `scroll_count` lines.  The `scroll_count` lines at the bottom
    /// the region are overwritten.  The `scroll_count` lines at the top
    /// of the region will become blank.
    ///
    /// After a region is scrolled, the cursor position is undefined,
    /// and the terminal's scroll region is set to the range specified.
    /// To restore scrolling behaviour to the full terminal window, an
    /// additional `Change::ScrollRegionDown { first_row: 0,
    /// region_size: height, scroll_count: 0 }`, where `height` is the
    /// height of the terminal, should be emitted.
    ScrollRegionDown {
        first_row: usize,
        region_size: usize,
        scroll_count: usize,
    },
    /// Change the title of the window in which the surface will be
    /// rendered.
    Title(String),

    /// Adjust the current line attributes, such as double height or width
    LineAttribute(LineAttribute),
}

impl Change {
    #[must_use]
    pub fn is_text(&self) -> bool {
        matches!(self, Change::Text(_))
    }

    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Change::Text(text) => text,
            _ => panic!("you must use Change::is_text() to guard calls to Change::text()"),
        }
    }
}

impl From<String> for Change {
    fn from(s: String) -> Self {
        Change::Text(s)
    }
}

impl From<&str> for Change {
    fn from(s: &str) -> Self {
        Change::Text(s.into())
    }
}

impl From<AttributeChange> for Change {
    fn from(c: AttributeChange) -> Self {
        Change::Attribute(c)
    }
}

impl From<LineAttribute> for Change {
    fn from(attr: LineAttribute) -> Self {
        Change::LineAttribute(attr)
    }
}

/// Keeps track of a run of changes and allows reasoning about the cursor
/// position and the extent of the screen that the sequence will affect.
/// This is useful for example when implementing something like a `LineEditor`
/// where you don't want to take control over the entire surface but do want
/// to be able to emit a dynamically sized output relative to the cursor
/// position at the time that the editor is invoked.
pub struct ChangeSequence {
    changes: Vec<Change>,
    screen_rows: usize,
    screen_cols: usize,
    pub(crate) cursor_x: usize,
    pub(crate) cursor_y: isize,
    render_y_max: isize,
    render_y_min: isize,
}

impl ChangeSequence {
    #[must_use]
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            changes: vec![],
            screen_rows: rows,
            screen_cols: cols,
            cursor_x: 0,
            cursor_y: 0,
            render_y_max: 0,
            render_y_min: 0,
        }
    }

    #[must_use]
    pub fn consume(self) -> Vec<Change> {
        self.changes
    }

    /// Returns the cursor position, (x, y).
    #[must_use]
    pub fn current_cursor_position(&self) -> (usize, isize) {
        (self.cursor_x, self.cursor_y)
    }

    pub fn move_to(&mut self, (cursor_x, cursor_y): (usize, isize)) {
        self.add(Change::CursorPosition {
            x: Position::Relative(cursor_x as isize - self.cursor_x as isize),
            y: Position::Relative(cursor_y - self.cursor_y),
        });
    }

    /// Returns the total number of rows affected
    #[must_use]
    pub fn render_height(&self) -> usize {
        (self.render_y_max - self.render_y_min)
            .max(0)
            .unsigned_abs()
    }

    fn update_render_height(&mut self) {
        self.render_y_max = self.render_y_max.max(self.cursor_y);
        self.render_y_min = self.render_y_min.min(self.cursor_y);
    }

    pub fn add_changes(&mut self, changes: Vec<Change>) {
        for change in changes {
            self.add(change);
        }
    }

    pub fn add<C: Into<Change>>(&mut self, change: C) {
        let change = change.into();
        match &change {
            Change::AllAttributes(_)
            | Change::Attribute(_)
            | Change::CursorColor(_)
            | Change::CursorShape(_)
            | Change::CursorVisibility(_)
            | Change::ClearToEndOfLine(_)
            | Change::Title(_)
            | Change::LineAttribute(_)
            | Change::ClearToEndOfScreen(_) => {}
            Change::Text(t) => {
                for g in Graphemes::new(t.as_str()) {
                    if self.cursor_x == self.screen_cols {
                        self.cursor_y += 1;
                        self.cursor_x = 0;
                    }
                    if g == "\n" {
                        self.cursor_y += 1;
                    } else if g == "\r" {
                        self.cursor_x = 0;
                    } else if g == "\r\n" {
                        self.cursor_y += 1;
                        self.cursor_x = 0;
                    } else {
                        let len = unicode_column_width(g, None);
                        self.cursor_x += len;
                    }
                }
                self.update_render_height();
            }
            Change::ClearScreen(_) => {
                self.cursor_x = 0;
                self.cursor_y = 0;
            }
            Change::CursorPosition { x, y } => {
                self.cursor_x = match x {
                    Position::Relative(x) => {
                        ((self.cursor_x as isize + x) % self.screen_cols as isize) as usize
                    }
                    Position::Absolute(x) => x % self.screen_cols,
                    Position::EndRelative(x) => (self.screen_cols - x) % self.screen_cols,
                };

                self.cursor_y = match y {
                    Position::Relative(y) => (self.cursor_y + y) % self.screen_rows as isize,
                    Position::Absolute(y) => (y % self.screen_rows) as isize,
                    Position::EndRelative(y) => {
                        ((self.screen_rows - y) % self.screen_rows) as isize
                    }
                };
                self.update_render_height();
            }
            Change::ScrollRegionUp { .. } | Change::ScrollRegionDown { .. } => {
                // The resultant cursor position is undefined by
                // the renderer!
                // We just pick something.
                self.cursor_x = 0;
                self.cursor_y = 0;
            }
        }

        self.changes.push(change);
    }
}


