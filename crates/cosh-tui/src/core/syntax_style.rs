use std::collections::HashMap;

use crate::core::rgba::{ColorInput, RGBA, parse_color};
use crate::core::utils::{TextAttributeOptions, create_text_attributes};

#[derive(Debug, Clone)]
pub struct StyleDefinition {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct StyleDefinitionInput {
    pub fg: Option<ColorInput>,
    pub bg: Option<ColorInput>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct MergedStyle {
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
}

#[derive(Debug, Clone)]
pub struct ThemeTokenStyle {
    pub scope: Vec<String>,
    pub style: ThemeTokenStyleInner,
}

#[derive(Debug, Clone)]
pub struct ThemeTokenStyleInner {
    pub foreground: Option<ColorInput>,
    pub background: Option<ColorInput>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

#[must_use]
pub fn convert_theme_to_styles(theme: &[ThemeTokenStyle]) -> HashMap<String, StyleDefinition> {
    let mut flat_styles = HashMap::new();

    for token_style in theme {
        let mut style_definition = StyleDefinition {
            fg: None,
            bg: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
        };

        if let Some(ref foreground) = token_style.style.foreground {
            style_definition.fg = Some(parse_color(foreground.clone()));
        }
        if let Some(ref background) = token_style.style.background {
            style_definition.bg = Some(parse_color(background.clone()));
        }

        if let Some(bold) = token_style.style.bold {
            style_definition.bold = Some(bold);
        }
        if let Some(italic) = token_style.style.italic {
            style_definition.italic = Some(italic);
        }
        if let Some(underline) = token_style.style.underline {
            style_definition.underline = Some(underline);
        }
        if let Some(dim) = token_style.style.dim {
            style_definition.dim = Some(dim);
        }

        for scope in &token_style.scope {
            flat_styles.insert(scope.clone(), style_definition.clone());
        }
    }

    flat_styles
}

pub struct SyntaxStyle {
    next_id: u64,
    name_cache: HashMap<String, u64>,
    style_defs: HashMap<String, StyleDefinition>,
    merged_cache: HashMap<String, MergedStyle>,
    destroyed: bool,
}

impl SyntaxStyle {
    #[must_use]
    pub fn create() -> Self {
        Self {
            next_id: 1,
            name_cache: HashMap::new(),
            style_defs: HashMap::new(),
            merged_cache: HashMap::new(),
            destroyed: false,
        }
    }

    #[must_use]
    pub fn from_theme(theme: &[ThemeTokenStyle]) -> Self {
        let mut style = Self::create();
        let flat_styles = convert_theme_to_styles(theme);
        for (name, style_def) in flat_styles {
            let input = StyleDefinitionInput {
                fg: style_def.fg.map(ColorInput::RGBA),
                bg: style_def.bg.map(ColorInput::RGBA),
                bold: style_def.bold,
                italic: style_def.italic,
                underline: style_def.underline,
                dim: style_def.dim,
            };
            let _ = style.register_style(&name, &input);
        }
        style
    }

    #[must_use]
    pub fn from_styles(styles: &HashMap<String, StyleDefinitionInput>) -> Self {
        let mut style = Self::create();
        for (name, style_def) in styles {
            let _ = style.register_style(name, style_def);
        }
        style
    }

    fn guard(&self) {
        assert!(!self.destroyed, "SyntaxStyle is destroyed");
    }

    #[must_use]
    pub fn register_style(&mut self, name: &str, style: &StyleDefinitionInput) -> u64 {
        self.guard();

        let fg = style.fg.as_ref().map(|c| parse_color(c.clone()));
        let bg = style.bg.as_ref().map(|c| parse_color(c.clone()));

        let id = self.next_id;
        self.next_id += 1;

        self.name_cache.insert(name.to_string(), id);
        self.style_defs.insert(
            name.to_string(),
            StyleDefinition {
                fg,
                bg,
                bold: style.bold,
                italic: style.italic,
                underline: style.underline,
                dim: style.dim,
            },
        );

        id
    }

    #[must_use]
    pub fn resolve_style_id(&self, name: &str) -> Option<u64> {
        self.guard();

        // Check cache first
        self.name_cache.get(name).copied()
    }

    #[must_use]
    pub fn get_style_id(&self, name: &str) -> Option<u64> {
        self.guard();

        if let Some(id) = self.name_cache.get(name) {
            return Some(*id);
        }

        // Try base name if it's a scoped style
        if let Some(dot_pos) = name.find('.') {
            let base_name = &name[..dot_pos];
            return self.name_cache.get(base_name).copied();
        }

        None
    }

    #[must_use]
    pub fn get_style_count(&self) -> usize {
        self.guard();
        self.style_defs.len()
    }

    pub fn clear_name_cache(&mut self) {
        self.name_cache.clear();
    }

    #[must_use]
    pub fn get_style(&self, name: &str) -> Option<&StyleDefinition> {
        self.guard();

        if let Some(style) = self.style_defs.get(name) {
            return Some(style);
        }

        if let Some(dot_pos) = name.find('.') {
            let base_name = &name[..dot_pos];
            return self.style_defs.get(base_name);
        }

        None
    }

    #[must_use]
    pub fn merge_styles(&mut self, style_names: &[&str]) -> MergedStyle {
        self.guard();

        let cache_key = style_names.join(":");
        if let Some(cached) = self.merged_cache.get(&cache_key) {
            return cached.clone();
        }

        let mut merged_def = StyleDefinition {
            fg: None,
            bg: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
        };

        for &name in style_names {
            if let Some(style) = self.get_style(name) {
                if style.fg.is_some() {
                    merged_def.fg = style.fg;
                }
                if style.bg.is_some() {
                    merged_def.bg = style.bg;
                }
                if style.bold.is_some() {
                    merged_def.bold = style.bold;
                }
                if style.italic.is_some() {
                    merged_def.italic = style.italic;
                }
                if style.underline.is_some() {
                    merged_def.underline = style.underline;
                }
                if style.dim.is_some() {
                    merged_def.dim = style.dim;
                }
            }
        }

        let attributes = create_text_attributes(TextAttributeOptions {
            bold: merged_def.bold.unwrap_or(false),
            italic: merged_def.italic.unwrap_or(false),
            underline: merged_def.underline.unwrap_or(false),
            dim: merged_def.dim.unwrap_or(false),
            ..Default::default()
        });

        let merged = MergedStyle {
            fg: merged_def.fg,
            bg: merged_def.bg,
            attributes,
        };

        self.merged_cache.insert(cache_key, merged.clone());
        merged
    }

    pub fn clear_cache(&mut self) {
        self.guard();
        self.merged_cache.clear();
    }

    #[must_use]
    pub fn get_cache_size(&self) -> usize {
        self.guard();
        self.merged_cache.len()
    }

    #[must_use]
    pub fn get_all_styles(&self) -> HashMap<String, StyleDefinition> {
        self.guard();
        self.style_defs.clone()
    }

    #[must_use]
    pub fn get_registered_names(&self) -> Vec<String> {
        self.guard();
        self.style_defs.keys().cloned().collect()
    }

    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        self.name_cache.clear();
        self.style_defs.clear();
        self.merged_cache.clear();
    }
}
