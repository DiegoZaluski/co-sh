/// Find the byte offset of the start of the word immediately before `cursor_pos`.
///
/// Skips any whitespace first, then skips the non-whitespace word, returning
/// the byte offset of the word's first character (or the same position if
/// already at the start of the text).
pub fn find_word_start(text: &str, cursor_pos: usize) -> usize {
    let pos = cursor_pos.min(text.len());
    let mut i = pos;

    // Skip whitespace immediately before the cursor.
    while i > 0 {
        let prev = text.floor_char_boundary(i - 1);
        if prev >= i {
            break;
        }
        let ch = text[prev..i].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            i = prev;
        } else {
            break;
        }
    }

    // Skip the non-whitespace word.
    while i > 0 {
        let prev = text.floor_char_boundary(i - 1);
        if prev >= i {
            break;
        }
        let ch = text[prev..i].chars().next().unwrap_or(' ');
        if !ch.is_whitespace() {
            i = prev;
        } else {
            break;
        }
    }

    i
}

/// Find the byte offset of the start of the next word after `cursor_pos`.
///
/// Skips the current non-whitespace word first, then skips any whitespace,
/// returning the byte offset of the next word's first character (or the same
/// position if already at the end of the text).
pub fn find_word_end(text: &str, cursor_pos: usize) -> usize {
    let len = text.len();
    let pos = cursor_pos.min(len);
    let mut i = pos;

    // Advance by one UTF-8 character from byte offset `i`.
    let char_advance = |s: &str, offset: usize| {
        if offset >= s.len() {
            return s.len();
        }
        let c = s[offset..].chars().next().unwrap_or(' ');
        offset + c.len_utf8()
    };

    // Skip the current word (non-whitespace) forward.
    while i < len {
        let next = char_advance(text, i);
        if next <= i {
            break;
        }
        let ch = text[i..next].chars().next().unwrap_or(' ');
        if !ch.is_whitespace() {
            i = next;
        } else {
            break;
        }
    }

    // Skip whitespace to reach the start of the next word.
    while i < len {
        let next = char_advance(text, i);
        if next <= i {
            break;
        }
        let ch = text[i..next].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            i = next;
        } else {
            break;
        }
    }

    i
}

/// Move a byte-offset cursor one visual line up (if `up`) or down (if `down`)
/// within `text` wrapped at `cols` columns, mirroring the chat prompt's
/// `cursor_up`/`cursor_down` (char-based visual lines).
///
/// Returns the new byte offset, unchanged when already on the first/last line.
#[must_use]
pub fn move_visual_line(text: &str, cursor_byte: usize, cols: usize, up: bool) -> usize {
    if text.is_empty() || cols == 0 {
        return cursor_byte;
    }
    let cols = cols.max(1);
    let char_pos = text[..cursor_byte.min(text.len())].chars().count();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut acc = 0usize; // accumulated char count (includes newlines)
    let mut byte_off = 0usize;
    for (li, line) in lines.iter().enumerate() {
        let line_chars = line.chars().count();
        if char_pos < acc + line_chars + 1 {
            let offset = char_pos.saturating_sub(acc);
            let visual_col = offset % cols;
            if up {
                // Cursor at the newline after this line (not the last line):
                // move to the previous logical line preserving the column.
                if offset >= line_chars && li + 1 < lines.len() {
                    if li == 0 {
                        return cursor_byte;
                    }
                    let prev = lines[li - 1];
                    let col = visual_col.min(cols - 1).min(prev.chars().count().saturating_sub(1));
                    return byte_off - prev.len() - 1 + byte_in_line(prev, col);
                }
                let visual_line = offset / cols;
                if visual_line == 0 {
                    // Already on the first visual line of this logical line.
                    if li == 0 {
                        return cursor_byte;
                    }
                    let prev = lines[li - 1];
                    let col = visual_col.min(cols - 1).min(prev.chars().count().saturating_sub(1));
                    return byte_off - prev.len() - 1 + byte_in_line(prev, col);
                }
                let target = (visual_line - 1) * cols + visual_col.min(cols - 1);
                return byte_off + byte_in_line(line, target);
            } else {
                let visual_lines = line_chars.div_ceil(cols).max(1);
                if offset >= line_chars {
                    // Cursor at the newline → move to the start of the next line.
                    if li + 1 >= lines.len() {
                        return cursor_byte;
                    }
                    return byte_off + line.len() + 1;
                }
                let visual_line = offset / cols;
                if visual_line + 1 >= visual_lines {
                    // Last visual line of this logical line → next logical line.
                    if li + 1 >= lines.len() {
                        return cursor_byte;
                    }
                    let next = lines[li + 1];
                    let next_start = byte_off + line.len() + 1;
                    let col = visual_col.min(cols - 1).min(next.chars().count().saturating_sub(1));
                    return next_start + byte_in_line(next, col);
                }
                let target = (visual_line + 1) * cols + visual_col.min(cols - 1);
                return byte_off + byte_in_line(line, target);
            }
        }
        acc += line_chars + 1;
        byte_off += line.len() + 1;
    }
    cursor_byte
}

/// Byte offset (within `line`) of the character at char index `char_idx`,
/// clamped to the line's end.
fn byte_in_line(line: &str, char_idx: usize) -> usize {
    line.char_indices()
        .nth(char_idx)
        .map_or(line.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_start_empty_text() {
        assert_eq!(find_word_start("", 0), 0);
    }

    #[test]
    fn word_start_simple() {
        let text = "hello world";
        // cursor at space — word before starts at 'h' (pos 0)
        assert_eq!(find_word_start(text, 6), 0);
        // cursor at 'w' — same word starts at 'w' (pos 6)
        assert_eq!(find_word_start(text, 7), 6);
        // cursor at 'l' in 'world' — word start is 'w' (pos 6)
        assert_eq!(find_word_start(text, 10), 6);
    }

    #[test]
    fn word_start_at_beginning() {
        assert_eq!(find_word_start("hello", 0), 0);
    }

    #[test]
    fn word_start_skips_whitespace() {
        // cursor at end of trailing spaces — word before starts at 'h' (pos 0)
        assert_eq!(find_word_start("hello   ", 8), 0);
    }

    #[test]
    fn word_start_middle_of_word() {
        let text = "hello";
        assert_eq!(find_word_start(text, 4), 0); // at 'l' in 'hello', go to 'h'
    }

    #[test]
    fn word_end_empty_text() {
        assert_eq!(find_word_end("", 0), 0);
    }

    #[test]
    fn word_end_simple() {
        let text = "hello world";
        assert_eq!(find_word_end(text, 0), 6); // after 'hello', at space, then skip to 'w'
        assert_eq!(find_word_end(text, 2), 6); // mid 'hello', skip to after 'hello' + spaces
    }

    #[test]
    fn word_end_at_end() {
        assert_eq!(find_word_end("hello", 5), 5);
        assert_eq!(find_word_end("hello", 3), 5); // mid word, go to end
    }

    #[test]
    fn word_end_skips_whitespace() {
        assert_eq!(find_word_end("hello   world", 0), 8); // after 'hello' + spaces, at 'w'
    }

    #[test]
    fn word_end_from_middle() {
        let text = "hello world";
        assert_eq!(find_word_end(text, 2), 6); // mid 'hello', after word + space
    }

    #[test]
    fn word_start_unicode() {
        let text = "olá mundo";
        // 'á' is 2 bytes (C3 A1). find_word_start from the 'm' should go to 'á'
        let pos = find_word_start(text, 7); // 'á' starts at 2
        assert_eq!(&text[pos..], "mundo"); // verify it found 'm'
    }

    #[test]
    fn word_end_unicode() {
        let text = "olá mundo";
        let pos = find_word_end(text, 0); // after 'olá' + space = at 'm'
        assert_eq!(&text[pos..], "mundo"); // exactly at 'mundo'
    }

    #[test]
    fn visual_line_within_wrapped_logical_line() {
        // 13 chars at 11 cols → 2 visual lines: "aaaaaa bbbb" and "bb"
        let text = "aaaaaa bbbbbb";
        let end = text.len(); // 13
        // Up from the end moves to the previous visual line, same column.
        let up = move_visual_line(text, end, 11, true);
        assert_eq!(up, 2);
        // Down from there returns to the end.
        assert_eq!(move_visual_line(text, up, 11, false), end);
    }

    #[test]
    fn visual_line_first_and_last() {
        let text = "aaaaaa bbbbbb";
        // Already on the first visual line: up is a no-op.
        assert_eq!(move_visual_line(text, 0, 11, true), 0);
        // Already on the last visual line: down is a no-op.
        assert_eq!(move_visual_line(text, text.len(), 11, false), text.len());
    }

    #[test]
    fn visual_line_between_logical_lines() {
        let text = "ab\ncd";
        // Cursor at the end (after 'cd') → up goes to 'ab' column 1.
        assert_eq!(move_visual_line(text, 4, 10, true), 1);
        // Down from 'ab' column 1 goes to 'cd' column 1.
        assert_eq!(move_visual_line(text, 1, 10, false), 4);
        // Up from the first logical line is a no-op.
        assert_eq!(move_visual_line(text, 1, 10, true), 1);
    }

    #[test]
    fn visual_line_unicode() {
        let text = "olá mundo";
        // 'á' is 2 bytes. End of text (char 9) at cols 3 → up to visual
        // line 2, column 0 → char index 6 ('n', byte 7).
        assert_eq!(move_visual_line(text, text.len(), 3, true), 7);
        // Down from the last visual line is a no-op (mirrors the prompt).
        assert_eq!(move_visual_line(text, 7, 3, false), 7);
    }
}
