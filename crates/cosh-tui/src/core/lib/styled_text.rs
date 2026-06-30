use crate::core::rgba::RGBA;

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub attributes: u32,
    pub link: Option<UrlLink>,
}

#[derive(Debug, Clone)]
pub struct UrlLink {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct StyledText {
    pub chunks: Vec<TextChunk>,
}

#[must_use]
pub fn string_to_styled_text(content: &str) -> StyledText {
    StyledText {
        chunks: vec![TextChunk {
            text: content.to_string(),
            fg: None,
            bg: None,
            attributes: 0,
            link: None,
        }],
    }
}
