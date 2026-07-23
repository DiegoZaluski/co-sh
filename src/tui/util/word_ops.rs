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
}
