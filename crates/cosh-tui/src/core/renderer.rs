use std::time::Duration;

use ratatui::Terminal;
use ratatui::layout::Rect;
use ratatui::prelude::Backend;

use crate::core::layout::LayoutTree;
use crate::core::renderable::{Renderable, RootRenderable};
use crate::core::rgba::ColorInput;

pub enum ScreenMode {
    AlternateScreen,
    MainScreen,
    SplitFooter { footer_height: u16 },
}

pub enum ExternalOutputMode {
    CaptureStdout,
    Passthrough,
}

pub enum ConsoleMode {
    ConsoleOverlay,
    Disabled,
}

pub struct PixelResolution {
    pub width: u16,
    pub height: u16,
}

#[allow(clippy::struct_excessive_bools)]
pub struct RendererConfig {
    pub alternate_screen: bool,
    pub width: u16,
    pub height: u16,
    pub target_fps: u32,
    pub max_fps: u32,
    pub exit_on_ctrl_c: bool,
    pub clear_on_shutdown: bool,
    pub enable_mouse_movement: bool,
    pub use_mouse: bool,
    pub auto_focus: bool,
    pub background_color: Option<ColorInput>,
    pub screen_mode: ScreenMode,
    pub external_output_mode: ExternalOutputMode,
    pub console_mode: ConsoleMode,
    pub debounce_delay: Duration,
    pub memory_snapshot_interval: Duration,
    pub gather_stats: bool,
    pub max_stat_samples: usize,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig {
            alternate_screen: true,
            width: 80,
            height: 24,
            target_fps: 30,
            max_fps: 60,
            exit_on_ctrl_c: true,
            clear_on_shutdown: true,
            enable_mouse_movement: true,
            use_mouse: true,
            auto_focus: true,
            background_color: None,
            screen_mode: ScreenMode::AlternateScreen,
            external_output_mode: ExternalOutputMode::Passthrough,
            console_mode: ConsoleMode::Disabled,
            debounce_delay: Duration::from_millis(100),
            memory_snapshot_interval: Duration::from_secs(0),
            gather_stats: false,
            max_stat_samples: 300,
        }
    }
}

pub struct RendererFrameEvent {
    pub frame_id: u64,
}

pub struct RendererStats {
    pub fps: f64,
    pub frame_count: u64,
    pub frame_times: Vec<f64>,
    pub average_frame_time: f64,
    pub min_frame_time: f64,
    pub max_frame_time: f64,
}

pub struct Renderer<B: Backend> {
    terminal: Terminal<B>,
    config: RendererConfig,
    root: RootRenderable,
    layout_tree: LayoutTree,
    frame_count: u64,
    destroyed: bool,
}

impl<B: Backend> Renderer<B> {
    pub fn new(terminal: Terminal<B>, config: RendererConfig) -> Self {
        Renderer {
            terminal,
            config,
            root: RootRenderable::new(),
            layout_tree: LayoutTree::new(),
            frame_count: 0,
            destroyed: false,
        }
    }

    pub fn root(&self) -> &RootRenderable {
        &self.root
    }

    pub fn root_mut(&mut self) -> &mut RootRenderable {
        &mut self.root
    }

    pub fn config(&self) -> &RendererConfig {
        &self.config
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn layout(&mut self, width: f32, height: f32) {
        self.layout_tree = LayoutTree::new();

        let root_style = self.root.build_style().unwrap_or_default();
        let root_id = self.layout_tree.new_leaf(root_style);
        self.root.set_layout_node(Some(root_id));

        fn build_tree(
            lt: &mut LayoutTree,
            node: &mut dyn Renderable,
            parent_id: taffy::NodeId,
        ) {
            for child in node.children_mut().iter_mut() {
                let style = child.build_style().unwrap_or_default();
                let child_id = lt.new_leaf(style);
                child.set_layout_node(Some(child_id));
                lt.add_child(parent_id, child_id);
                build_tree(lt, child.as_mut(), child_id);
            }
        }

        build_tree(&mut self.layout_tree, &mut self.root, root_id);
        self.layout_tree.compute_layout(width, height);

        fn apply_lt(lt: &LayoutTree, node: &mut dyn Renderable) {
            if let Some(nid) = node.layout_node() {
                let layout = lt.layout(nid);
                node.apply_layout(layout);
            }
            for child in node.children_mut().iter_mut() {
                apply_lt(lt, child.as_mut());
            }
        }

        apply_lt(&self.layout_tree, &mut self.root);
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn render_frame(&mut self, _delta_time: f64) -> Result<(), B::Error> {
        let area = self.terminal.size()?;
        self.layout(area.width as f32, area.height as f32);
        self.terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            self.root.render_self(buf, area);
        })?;
        self.frame_count = self.frame_count.wrapping_add(1);
        Ok(())
    }

    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.terminal.resize(Rect::new(0, 0, width, height)).ok();
    }
}
