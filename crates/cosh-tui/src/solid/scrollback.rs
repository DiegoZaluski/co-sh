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
        SolidScrollbackWriterOptions {
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
pub type ScrollbackWriter =
    Box<dyn Fn(ScrollbackRenderContext) -> ScrollbackSnapshot>;

/// Context provided to the scrollback writer.
pub struct ScrollbackRenderContext {
    pub width: u16,
    pub tail_column: u16,
}

/// Create a scrollback writer from a closure.
/// Stub: full implementation requires `BoxRenderable` and the `SolidJS` reconciler.
#[must_use]
pub fn _create_scrollback_writer(
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
) -> ScrollbackWriter {
    Box::new(|_ctx: ScrollbackRenderContext| -> ScrollbackSnapshot {
        unimplemented!("Scrollback writer requires BoxRenderable (item 20)")
    })
}

/// Write a solid component tree to the renderer's scrollback.
pub fn _write_solid_to_scrollback(
    _renderer: (),
    _node: Box<dyn Fn(&ScrollbackRenderContext)>,
    _options: SolidScrollbackWriterOptions,
) {
    unimplemented!("write_solid_to_scrollback requires the scrollback writer")
}
