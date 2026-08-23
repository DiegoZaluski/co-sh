//! LSP position ↔ byte-offset conversions.
//!
//! All conversions live here so the rest of the engine never hand-rolls the
//! math (crush shipped an off-by-one contract confusion doing exactly that).
//!
//! LSP positions are line/character pairs where `character` counts **UTF-16
//! code units** unless the server negotiated `utf-8`, in which case it counts
//! bytes. `\n`, `\r\n` and lone `\r` all terminate a line.

use lsp_types::Position;

/// How `character` offsets are counted on the wire, negotiated at initialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionEncoding {
    /// `character` counts UTF-16 code units (protocol default).
    Utf16,
    /// `character` counts bytes (only when the server picked our offer).
    Utf8,
}

/// Convert a byte offset into an LSP [`Position`] under `encoding`.
///
/// Returns `None` when `offset` is out of bounds or lands mid-character.
pub fn offset_to_position(
    text: &str,
    offset: usize,
    encoding: PositionEncoding,
) -> Option<Position> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }

    let mut line = 0u32;
    let mut character = 0u32;
    let mut consumed = 0usize;

    for ch in text[..offset].chars() {
        consumed += ch.len_utf8();
        match ch {
            '\n' => {
                line += 1;
                character = 0;
            }
            // A `\r` before a `\n` belongs to that same break (the `\n` arm
            // does the bump); a lone `\r` terminates its own line.
            '\r' if !text[consumed..].starts_with('\n') => {
                line += 1;
                character = 0;
            }
            _ => {
                character += match encoding {
                    PositionEncoding::Utf8 => ch.len_utf8() as u32,
                    PositionEncoding::Utf16 => ch.len_utf16() as u32,
                };
            }
        }
    }

    Some(Position { line, character })
}

/// Convert an LSP [`Position`] into a byte offset under `encoding`.
///
/// Out-of-range lines/characters clamp to the nearest valid boundary instead
/// of failing: servers routinely send positions computed against stale
/// content, and failing those would turn a stale answer into a hard error.
pub fn position_to_offset(text: &str, position: Position, encoding: PositionEncoding) -> usize {
    let bounds = line_bounds(text);
    let (start, end) = match bounds.get(position.line as usize) {
        Some(&bounds) => bounds,
        None => return text.len(),
    };

    let mut units = 0u32;
    for (idx, ch) in text[start..end].char_indices() {
        if units >= position.character {
            return start + idx;
        }
        units += match encoding {
            PositionEncoding::Utf8 => ch.len_utf8() as u32,
            PositionEncoding::Utf16 => ch.len_utf16() as u32,
        };
    }
    end
}

/// Byte range of each line's *content* (terminators excluded), per LSP line
/// semantics (`\n`, `\r\n`, lone `\r`). A trailing terminator yields a final
/// empty line; so does empty input.
fn line_bounds(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut bounds = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                bounds.push((start, i));
                i += 1;
                start = i;
            }
            b'\r' => {
                bounds.push((start, i));
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    bounds.push((start, text.len()));
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes: a(0) b(1) \n(2) | c(3) d(4) 😀(5–8) e(9) \n(10) | f(11); len 12.
    const SAMPLE: &str = "ab\ncd\u{1F600}e\nf";

    #[test]
    fn utf16_roundtrip_ascii() {
        let pos = offset_to_position(SAMPLE, 3, PositionEncoding::Utf16).unwrap();
        assert_eq!(
            pos,
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(position_to_offset(SAMPLE, pos, PositionEncoding::Utf16), 3);
    }

    #[test]
    fn utf16_counts_surrogate_pairs() {
        // 'e' sits after "cd😀": 4 UTF-16 units into line 1, byte 9 overall.
        let pos = offset_to_position(SAMPLE, 9, PositionEncoding::Utf16).unwrap();
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 4);

        assert_eq!(
            position_to_offset(
                SAMPLE,
                Position {
                    line: 1,
                    character: 3
                },
                PositionEncoding::Utf16
            ),
            9,
            "character 3 falls inside the surrogate pair → snaps past the emoji"
        );
    }

    #[test]
    fn utf8_counts_bytes() {
        let pos = offset_to_position(SAMPLE, 9, PositionEncoding::Utf8).unwrap();
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 6);

        assert_eq!(
            position_to_offset(
                SAMPLE,
                Position {
                    line: 1,
                    character: 4
                },
                PositionEncoding::Utf8
            ),
            9,
            "byte offset 4 lands inside the emoji → snaps to its end"
        );
    }

    #[test]
    fn out_of_range_clamps() {
        assert_eq!(
            position_to_offset(
                SAMPLE,
                Position {
                    line: 99,
                    character: 0
                },
                PositionEncoding::Utf16
            ),
            SAMPLE.len()
        );
        assert_eq!(
            position_to_offset(
                SAMPLE,
                Position {
                    line: 2,
                    character: 999
                },
                PositionEncoding::Utf16
            ),
            SAMPLE.len(),
            "past end of final line content"
        );
        assert_eq!(
            position_to_offset(
                SAMPLE,
                Position {
                    line: 1,
                    character: 999
                },
                PositionEncoding::Utf16
            ),
            10,
            "past end of a middle line clamps to its terminator"
        );
    }

    #[test]
    fn invalid_offsets_rejected() {
        assert!(offset_to_position(SAMPLE, 9999, PositionEncoding::Utf16).is_none());
        assert!(
            offset_to_position(SAMPLE, 6, PositionEncoding::Utf16).is_none(),
            "mid-character (inside the emoji)"
        );
    }

    #[test]
    fn crlf_and_lone_cr_line_endings() {
        let text = "a\r\nb\r";
        assert_eq!(
            offset_to_position(text, 3, PositionEncoding::Utf16),
            Some(Position {
                line: 1,
                character: 0
            })
        );
        assert_eq!(
            position_to_offset(
                text,
                Position {
                    line: 1,
                    character: 1
                },
                PositionEncoding::Utf16
            ),
            4
        );
        assert_eq!(
            position_to_offset(
                text,
                Position {
                    line: 2,
                    character: 0
                },
                PositionEncoding::Utf16
            ),
            5
        );
    }

    #[test]
    fn trailing_newline_yields_final_empty_line() {
        let text = "x\n";
        assert_eq!(
            position_to_offset(
                text,
                Position {
                    line: 1,
                    character: 0
                },
                PositionEncoding::Utf16
            ),
            2
        );
        assert_eq!(
            offset_to_position(text, 2, PositionEncoding::Utf16),
            Some(Position {
                line: 1,
                character: 0
            })
        );
    }

    #[test]
    fn empty_text() {
        assert_eq!(
            offset_to_position("", 0, PositionEncoding::Utf16),
            Some(Position {
                line: 0,
                character: 0
            })
        );
        assert_eq!(
            position_to_offset(
                "",
                Position {
                    line: 0,
                    character: 0
                },
                PositionEncoding::Utf16
            ),
            0
        );
        assert_eq!(
            position_to_offset(
                "",
                Position {
                    line: 5,
                    character: 5
                },
                PositionEncoding::Utf16
            ),
            0
        );
    }

    #[test]
    fn roundtrip_all_char_boundaries_utf16() {
        let boundaries = [0usize, 1, 2, 3, 4, 5, 9, 10, 11, 12];
        for offset in boundaries {
            let Some(pos) = offset_to_position(SAMPLE, offset, PositionEncoding::Utf16) else {
                continue;
            };
            assert_eq!(
                position_to_offset(SAMPLE, pos, PositionEncoding::Utf16),
                offset,
                "roundtrip failed at byte {offset}"
            );
        }
    }
}
