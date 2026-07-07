/// Estimate the number of tokens in a text string without calling any API.
///
/// Uses a character-run heuristic that mimics BPE tokenizer behaviour:
///
/// | Category   | Estimate                     |
/// |------------|------------------------------|
/// | Letters ≤5 | 1 token                      |
/// | Letters >5 | 1 + ⌈(n−5) / 4⌉              |
/// | Digits     | max(1, ⌈n / 3⌉)              |
/// | Punct      | max(1, ⌈n / 2⌉)              |
/// | Other (CJK)| max(1, ⌈n / 2⌉)              |
///
/// Structural whitespace (newlines) adds a small overhead.
///
/// # Examples
/// ```
/// # use cosh::util::token_counter::estimate_tokens;
/// assert_eq!(estimate_tokens("hello world"), 2);
/// assert_eq!(estimate_tokens(""), 0);
/// assert_eq!(estimate_tokens("hello, how are you"), 5);
/// ```
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut count = 0usize;

    for word in text.split_whitespace() {
        count += tokens_for_word(word);
    }

    // Paragraph breaks and indentation can introduce extra tokens
    // in SentencePiece / BPE encodings.
    let newlines = text.bytes().filter(|&b| b == b'\n').count();
    if newlines > 1 {
        count += newlines.saturating_sub(1) / 2;
    }

    count
}

//.......... Character-run tokeniser ...

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharCat {
    Letter,
    Digit,
    Punct,
    Other,
}

fn char_category(c: char) -> CharCat {
    if c.is_ascii_alphabetic() {
        CharCat::Letter
    } else if c.is_ascii_digit() {
        CharCat::Digit
    } else if c.is_ascii_punctuation() {
        CharCat::Punct
    } else {
        CharCat::Other
    }
}

/// Estimate tokens for a single whitespace-delimited word by splitting it into
/// contiguous runs of the same character category (letters, digits, punct, other)
/// and estimating each run independently.
fn tokens_for_word(word: &str) -> usize {
    let chars: Vec<char> = word.chars().collect();
    if chars.is_empty() {
        return 0;
    }

    let mut count = 0usize;
    let mut i = 0;

    while i < chars.len() {
        let cat = char_category(chars[i]);
        let start = i;
        while i < chars.len() && char_category(chars[i]) == cat {
            i += 1;
        }
        let n = i - start;

        count += match cat {
            CharCat::Letter => {
                if n <= 5 {
                    1
                } else {
                    1 + (n - 5).div_ceil(4)
                }
            }
            CharCat::Digit => n.div_ceil(3).max(1),
            CharCat::Punct | CharCat::Other => n.div_ceil(2).max(1),
        };
    }

    count.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn simple_phrases() {
        assert_eq!(estimate_tokens("ola, como vai"), 4);
        assert_eq!(estimate_tokens("hello world"), 2);
        assert_eq!(estimate_tokens("foo"), 1);
    }

    #[test]
    fn numbers() {
        assert_eq!(estimate_tokens("123"), 1);
        assert_eq!(estimate_tokens("123456"), 2);
        assert_eq!(estimate_tokens("123456789"), 3);
    }

    #[test]
    fn mixed_numbers_and_text() {
        // "abc" (1) + "123" (1) + "xyz" (1) = 3
        assert_eq!(estimate_tokens("abc 123 xyz"), 3);
    }

    #[test]
    fn long_words() {
        let count = estimate_tokens("supercalifragilisticexpialidocious");
        assert!(count > 3, "long words should count as >1 token: {count}");
        // 34 letters → 1 + ceil((34 - 5) / 4) = 1 + ceil(29/4) = 1 + 8 = 9
        assert_eq!(count, 9);
    }

    #[test]
    fn mixed_length() {
        // "a b c" = 3, "supercalifragilisticexpialidocious" = 9 → total 12
        let count = estimate_tokens("a b c supercalifragilisticexpialidocious");
        assert_eq!(count, 12);
    }

    #[test]
    fn punctuation_splitting() {
        assert_eq!(estimate_tokens("foo,bar"), 3); // foo + , + bar
        assert_eq!(estimate_tokens("don't"), 3); // don + ' + t (heuristic)
        assert_eq!(estimate_tokens("end."), 2); // end + .
    }

    #[test]
    fn newline_overhead() {
        let single = estimate_tokens("hello world");
        let multi = estimate_tokens("hello\n\nworld");
        assert!(multi >= single, "newlines should not reduce the estimate");
    }

    #[test]
    fn unicode() {
        let count = estimate_tokens("こんにちは世界");
        assert!(count >= 2, "unicode string should produce tokens: {count}");
    }

    #[test]
    fn punctuation_runs() {
        assert_eq!(estimate_tokens("!!"), 1); // len 2 → 2/2 = 1
        assert_eq!(estimate_tokens("..."), 2); // len 3 → ceil(3/2) = 2
        assert_eq!(estimate_tokens("...!"), 2); // punct run of 4 → ceil(4/2) = 2
    }
}
