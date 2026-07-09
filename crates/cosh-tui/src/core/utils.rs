use crate::core::types::TextAttributes;

/// Options for `create_text_attributes`, matching the TS destructured options object.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TextAttributeOptions {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

#[must_use]
pub const fn create_text_attributes(options: TextAttributeOptions) -> u32 {
    let mut attributes = TextAttributes::NONE.bits();
    if options.bold {
        attributes |= TextAttributes::BOLD.bits();
    }
    if options.italic {
        attributes |= TextAttributes::ITALIC.bits();
    }
    if options.underline {
        attributes |= TextAttributes::UNDERLINE.bits();
    }
    if options.dim {
        attributes |= TextAttributes::DIM.bits();
    }
    if options.blink {
        attributes |= TextAttributes::BLINK.bits();
    }
    if options.inverse {
        attributes |= TextAttributes::INVERSE.bits();
    }
    if options.hidden {
        attributes |= TextAttributes::HIDDEN.bits();
    }
    if options.strikethrough {
        attributes |= TextAttributes::STRIKETHROUGH.bits();
    }
    attributes
}

// Link attribute helpers (bits 8-31 encode link_id)
const ATTRIBUTE_BASE_MASK: u32 = 0xff;
const LINK_ID_SHIFT: u32 = 8;
const LINK_ID_PAYLOAD_MASK: u32 = 0xff_ff_ff;

#[must_use]
pub const fn attributes_with_link(base_attributes: u32, link_id: u32) -> u32 {
    let base = base_attributes & ATTRIBUTE_BASE_MASK;
    let link_bits = (link_id & LINK_ID_PAYLOAD_MASK) << LINK_ID_SHIFT;
    base | link_bits
}

#[must_use]
pub const fn get_link_id(attributes: u32) -> u32 {
    (attributes >> LINK_ID_SHIFT) & LINK_ID_PAYLOAD_MASK
}
