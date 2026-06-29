use crate::core::types::TextAttributes;

pub fn create_text_attributes(
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
) -> u32 {
    let mut attributes = TextAttributes::NONE.bits();
    if bold { attributes |= TextAttributes::BOLD.bits(); }
    if italic { attributes |= TextAttributes::ITALIC.bits(); }
    if underline { attributes |= TextAttributes::UNDERLINE.bits(); }
    if dim { attributes |= TextAttributes::DIM.bits(); }
    if blink { attributes |= TextAttributes::BLINK.bits(); }
    if inverse { attributes |= TextAttributes::INVERSE.bits(); }
    if hidden { attributes |= TextAttributes::HIDDEN.bits(); }
    if strikethrough { attributes |= TextAttributes::STRIKETHROUGH.bits(); }
    attributes
}

// Link attribute helpers (bits 8-31 encode link_id)
const ATTRIBUTE_BASE_MASK: u32 = 0xff;
const LINK_ID_SHIFT: u32 = 8;
const LINK_ID_PAYLOAD_MASK: u32 = 0xffffff;

pub fn attributes_with_link(base_attributes: u32, link_id: u32) -> u32 {
    let base = base_attributes & ATTRIBUTE_BASE_MASK;
    let link_bits = (link_id & LINK_ID_PAYLOAD_MASK) << LINK_ID_SHIFT;
    base | link_bits
}

pub fn get_link_id(attributes: u32) -> u32 {
    (attributes >> LINK_ID_SHIFT) & LINK_ID_PAYLOAD_MASK
}
