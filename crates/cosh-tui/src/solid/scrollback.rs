use crate::core::renderable::RootRenderable;

/// Options for the scrollback writer.
pub struct SolidScrollbackWriterOptions {
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub row_columns: Option<u16>,
    pub start_on_new_line: Option<bool>,
    pub trailing_newline: Option<bool>,
}

impl Default for SolidScrollbackWriterOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            row_columns: None,
            start_on_new_line: Some(true),
            trailing_newline: None,
        }
    }
}

/// A scrollback snapshot produced by the writer.
pub struct ScrollbackSnapshot {
    pub root: RootRenderable,
    pub width: u16,
    pub height: u16,
    pub row_columns: Option<u16>,
    pub start_on_new_line: bool,
    pub trailing_newline: bool,
}

/// Writer function that produces a scrollback snapshot.
pub type ScrollbackWriter = Box<dyn Fn(ScrollbackRenderContext) -> ScrollbackSnapshot>;

/// Context provided to the scrollback writer.
pub struct ScrollbackRenderContext {
    pub width: u16,
    pub tail_column: u16,
}

/// Create a scrollback writer from a closure.
// TODO: integrate with ratatui backend and the LayoutTree once the renderer
// exposes a scrollback surface API equivalent to the original OptimizedBuffer path.
#[must_use]
pub fn create_scrollback_writer(
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
) -> ScrollbackWriter {
    Box::new(|ctx: ScrollbackRenderContext| -> ScrollbackSnapshot {
        ScrollbackSnapshot {
            root: RootRenderable::new(),
            width: ctx.width,
            height: 1,
            row_columns: None,
            start_on_new_line: true,
            trailing_newline: false,
        }
    })
}

/// Write a solid component tree to the renderer's scrollback.
// TODO: implement once the renderer exposes scrollback commit/teardown methods.
pub fn write_solid_to_scrollback(
    _renderer: (),
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
) {
}
