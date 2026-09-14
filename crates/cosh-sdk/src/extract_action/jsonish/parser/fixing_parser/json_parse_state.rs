use std::iter::Peekable;

use super::json_collection::JsonCollection;
use crate::extract_action::jsonish::{
    Value, error::JsonishError, value::CompletionState, value::Fixes,
};

/// Tracks quote and backslash state incrementally for quoted strings
/// to avoid O(n²) rescanning when determining if a quote closes a string.
#[derive(Debug, Default, Clone)]
struct StringQuoteTracking {
    /// Number of consecutive backslashes at the end of the current string content.
    /// Used to determine if a quote is escaped.
    trailing_backslashes: usize,
    /// Count of unescaped quotes (quotes preceded by an even number of backslashes).
    /// Used in `should_close_string` to decide whether to close.
    unescaped_quote_count: usize,
}

/// Incremental JSON parser state machine that assembles JSON values from a
/// stream of characters. Handles malformed JSON commonly produced by LLMs,
/// including unquoted strings, single-quoted strings, trailing commas, and
/// unterminated structures.
#[derive(Debug)]
pub struct JsonParseState {
    /// The stack of Json collection values being assembled.
    /// The stack-ness is used in order to parse nested values,
    /// e.g. an object with fields of list, or lists of lists.
    pub collection_stack: Vec<(JsonCollection, Vec<Fixes>)>,

    /// Values for which parsing is completed, and popped off of the
    /// collection stack.
    /// Technically we may find multiple values in a single string
    pub completed_values: Vec<(&'static str, Value, Vec<Fixes>)>,

    /// Incremental tracking state for the current quoted string being parsed.
    /// Reset when a new string is started, used to avoid O(n²) quote counting.
    string_quote_tracking: StringQuoteTracking,
}

/// The position context of the current unquoted string relative to its
/// enclosing collection. Used by `should_close_unescaped_string` to determine
/// which delimiters terminate the string.
#[derive(Clone, Debug)]
enum Pos {
    InNothing,     // 0
    Unknown,       // 1
    InObjectKey,   // 2
    InObjectValue, // 3
    InArray,       // 4
}

impl JsonParseState {
    /// Creates a new empty parser state with no collections on the stack.
    pub fn new() -> Self {
        Self {
            collection_stack: vec![],
            completed_values: vec![],
            string_quote_tracking: StringQuoteTracking::default(),
        }
    }

    /// Reset the quote tracking state when starting a new quoted string
    fn reset_quote_tracking(&mut self) {
        self.string_quote_tracking = StringQuoteTracking::default();
    }

    /// Update quote tracking when consuming a character into a quoted string.
    /// Must be called BEFORE the character is added to the string.
    const fn update_quote_tracking(&mut self, token: char) {
        if token == '\\' {
            self.string_quote_tracking.trailing_backslashes += 1;
        } else {
            if token == '"' {
                // A quote is "unescaped" if preceded by an even number of backslashes
                if self
                    .string_quote_tracking
                    .trailing_backslashes
                    .is_multiple_of(2)
                {
                    self.string_quote_tracking.unescaped_quote_count += 1;
                }
            }
            self.string_quote_tracking.trailing_backslashes = 0;
        }
    }

    /// Examine the top of the collection stack, popping it off and
    /// adding it to `completed_values` if it is ready.
    ///
    /// The `completion_state` parameter is applied to the value being
    /// completed. If it is `CompletionState::Complete`, we also apply
    /// that state to the children of the value being completed.
    #[allow(clippy::panic)]
    pub fn complete_collection(&mut self, completion_state: CompletionState) {
        let Some((collection, fixes)) = self.collection_stack.pop() else {
            return;
        };

        let name = collection.name();

        let mut value: Value = match collection.into() {
            Some(value) => value,
            None => return,
        };
        if completion_state == CompletionState::Complete {
            value.complete_deeply();
        }

        if let Some((last, _fixes)) = self.collection_stack.last_mut() {
            match last {
                JsonCollection::Object(keys, values, _) => {
                    if keys.len() == values.len() {
                        match value {
                            Value::String(s, _) | Value::AnyOf(_, s) => keys.push(s),
                            _ => keys.push(value.to_string()),
                        }
                    } else {
                        values.push(value);
                    }
                }
                JsonCollection::Array(values, _) => {
                    values.push(value);
                }
                _ => {
                    // TODO: this should never happen as we should only be pushing objects and arrays
                    panic!("Unexpected value: {value:?} in collection stack: {last:?}");
                }
            }
        } else {
            self.completed_values.push((name, value, fixes));
        }
    }

    /// Appends a character to the current string-like collection on top of the stack.
    /// Returns `Ok(0)` on success (no additional characters to skip).
    #[allow(clippy::panic)]
    fn consume(&mut self, token: char) -> Result<usize, JsonishError> {
        // First check if we're in a QuotedString and need to update tracking
        // (done before getting mutable borrow to avoid borrow checker conflict)
        let is_quoted_string = matches!(
            self.collection_stack.last(),
            Some((JsonCollection::QuotedString(..), _))
        );
        if is_quoted_string {
            // Track quote/backslash state incrementally for O(1) quote counting
            self.update_quote_tracking(token);
        }

        // Now get mutable access to push the token
        let Some((last, _)) = self.collection_stack.last_mut() else {
            return Err(JsonishError(format!(
                "No collection to consume token: {token:?}"
            )));
        };
        match last {
            JsonCollection::QuotedString(s, _)
            | JsonCollection::TripleQuotedString(s, _)
            | JsonCollection::BlockComment(s, _)
            | JsonCollection::SingleQuotedString(s, _)
            | JsonCollection::BacktickString(s, _)
            | JsonCollection::TripleBacktickString {
                content: (s, _), ..
            }
            | JsonCollection::UnquotedString(s, _)
            | JsonCollection::TrailingComment(s, _) => {
                // println!("Consuming: {s} + {:?}", token);
                s.push(token);
            }
            JsonCollection::Object(_, _, _) | JsonCollection::Array(_, _) => {
                panic!("Unexpected token: {token:?} in: {last:?}");
            }
        }
        Ok(0)
    }

    /// Returns `true` if the current unquoted string on the stack represents a
    /// complete JSON literal (`true`, `false`, `null`, or a valid number).
    #[allow(dead_code)]
    fn is_string_complete(&self) -> bool {
        let Some((JsonCollection::UnquotedString(v, _), _)) = self.collection_stack.last() else {
            return false;
        };

        // Check if the token is a valid json character
        match v.as_str() {
            "true" | "false" | "null" => true,
            _ => {
                // Check if the token parses as a number
                if v.parse::<f64>().is_ok() {
                    return true;
                }
                false
            }
        }
    }

    /// Determines whether the current unquoted string should be closed based on
    /// upcoming characters. Consumes characters from `next` looking for a
    /// structural delimiter appropriate to the string's context (`:` for object
    /// keys, `,`/`}` for object values, `,`/`]` for arrays, `{`/`[` for
    /// top-level). Returns the number of characters consumed so the caller can
    /// advance the outer iterator accordingly.
    ///
    /// When the iterator is exhausted without finding a delimiter (stream
    /// incomplete), returns `Close(counter, Incomplete)` where `counter` is the
    /// number of characters consumed. The `counter += 1` before each such return
    /// accounts for the last character yielded by the iterator — without it, the
    /// outer loop would re-process that character, causing duplication.
    fn should_close_unescaped_string(
        &mut self,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> CloseStringResult {
        let pos = self.unescaped_string_position();
        match pos {
            Pos::InNothing => self.close_unescaped_in_nothing(next),
            Pos::Unknown => CloseStringResult::Continue,
            Pos::InObjectKey => self.close_unescaped_in_object_key(next),
            Pos::InObjectValue => self.close_unescaped_in_object_value(next),
            Pos::InArray => self.close_unescaped_in_array(next),
        }
    }

    #[allow(clippy::unwrap_used)]
    fn unescaped_string_position(&self) -> Pos {
        if self.collection_stack.len() >= 2 {
            self.collection_stack
                .get(self.collection_stack.len() - 2)
                .map(|(c, _)| match c {
                    JsonCollection::Object(keys, values, _) => {
                        if keys.len() == values.len() {
                            Pos::InObjectKey
                        } else {
                            Pos::InObjectValue
                        }
                    }
                    JsonCollection::Array(_, _) => Pos::InArray,
                    _ => Pos::Unknown,
                })
                .unwrap()
        } else {
            Pos::InNothing
        }
    }

    fn close_unescaped_in_nothing(
        &mut self,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> CloseStringResult {
        let mut counter = 0;
        for (idx, c) in next.by_ref() {
            counter = idx;
            match c {
                '{' | '[' => return CloseStringResult::Close(idx, CompletionState::Complete),
                x => {
                    let _ = self.consume(x);
                }
            }
        }
        counter += 1;
        CloseStringResult::Close(counter, CompletionState::Incomplete)
    }

    fn close_unescaped_in_object_key(
        &mut self,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> CloseStringResult {
        let mut counter = 0;
        for (idx, c) in next.by_ref() {
            counter = idx;
            match c {
                ':' => return CloseStringResult::Close(idx, CompletionState::Complete),
                x => {
                    let _ = self.consume(x);
                }
            }
        }
        counter += 1;
        CloseStringResult::Close(counter, CompletionState::Incomplete)
    }

    fn close_unescaped_in_object_value(
        &mut self,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> CloseStringResult {
        let mut counter = 0;
        while let Some((idx, c)) = next.next() {
            counter = idx;
            match c {
                ',' => {
                    let Some((JsonCollection::UnquotedString(current_value, _), _)) =
                        self.collection_stack.last()
                    else {
                        return CloseStringResult::Close(idx, CompletionState::Complete);
                    };

                    let is_numeric = current_value.trim().parse::<f64>().is_ok();
                    let is_bool = current_value.trim().eq_ignore_ascii_case("true")
                        || current_value.trim().eq_ignore_ascii_case("false");
                    let is_null = current_value.trim().eq_ignore_ascii_case("null");
                    let is_identifier =
                        !(current_value.contains(' ') || current_value.contains('('));
                    let is_possible_value = is_numeric || is_bool || is_null || is_identifier;

                    if let Some((_, next_c)) = next.peek() {
                        match next_c {
                            '\n' => {
                                log::debug!("Closing due to: newline after comma");
                                return CloseStringResult::Close(idx, CompletionState::Complete);
                            }
                            ' ' => {
                                log::debug!("Testing for comment after space + comma");
                                if is_possible_value {
                                    return CloseStringResult::Close(
                                        idx,
                                        CompletionState::Complete,
                                    );
                                }
                                let mut buffer = ",".to_string();
                                let mut anything_but_whitespace = false;
                                while let Some((_, next_next_c)) = next.next() {
                                    anything_but_whitespace =
                                        anything_but_whitespace || !next_next_c.is_whitespace();
                                    buffer.push(next_next_c);
                                    match next_next_c {
                                        ' ' => {}
                                        '\n' => {
                                            if !anything_but_whitespace {
                                                log::debug!(
                                                    "Closing due to: newline after comma + space"
                                                );
                                                return CloseStringResult::Close(
                                                    idx,
                                                    CompletionState::Complete,
                                                );
                                            }
                                        }
                                        '/' => {
                                            if matches!(next.peek(), Some((_, '/' | '*'))) {
                                                return CloseStringResult::Close(
                                                    idx,
                                                    CompletionState::Complete,
                                                );
                                            }
                                        }
                                        '"' => {
                                            log::debug!(
                                                "Closing due to: new key after space + comma"
                                            );
                                            return CloseStringResult::Close(
                                                idx,
                                                CompletionState::Complete,
                                            );
                                        }
                                        _x => {
                                            break;
                                        }
                                    }
                                }
                                for c in buffer.chars() {
                                    let _ = self.consume(c);
                                }
                            }
                            _ => {
                                let _ = self.consume(c);
                            }
                        }
                    } else {
                        return CloseStringResult::Close(idx, CompletionState::Complete);
                    }
                }
                '}' => return CloseStringResult::Close(idx, CompletionState::Complete),
                x => {
                    let _ = self.consume(x);
                }
            }
        }
        counter += 1;
        CloseStringResult::Close(counter, CompletionState::Incomplete)
    }

    fn close_unescaped_in_array(
        &mut self,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> CloseStringResult {
        let mut counter = 0;
        for (idx, c) in next {
            counter = idx;
            match c {
                ',' | ']' => return CloseStringResult::Close(idx, CompletionState::Complete),
                x => {
                    let _ = self.consume(x);
                }
            }
        }
        counter += 1;
        CloseStringResult::Close(counter, CompletionState::Incomplete)
    }

    /// Determines whether a quoted string (double-quoted, single-quoted, or
    /// backtick) should be closed at the current position by peeking at
    /// upcoming characters and checking for structural delimiters.
    #[allow(clippy::unwrap_used)]
    fn should_close_string(
        &self,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
        closing_char: char,
    ) -> bool {
        let (has_some_object, in_object_key, in_object_value, in_array) =
            if self.collection_stack.len() >= 2 {
                self.collection_stack
                    .get(self.collection_stack.len() - 2)
                    .map(|(c, _)| match c {
                        JsonCollection::Object(keys, values, _) => {
                            if keys.len() == values.len() {
                                (true, false, false)
                            } else {
                                (false, true, true)
                            }
                        }
                        JsonCollection::Array(_, _) => (false, false, true),
                        _ => (false, false, false),
                    })
                    .map(|(a, b, c)| (true, a, b, c))
                    .unwrap()
            } else {
                (false, false, false, false)
            };
        // Use pre-computed quote count from incremental tracking (O(1) instead of O(n²))
        let closing_char_count = if closing_char == '"' {
            #[allow(clippy::unwrap_used)]
            let (last, _) = self.collection_stack.last().unwrap();
            match last {
                JsonCollection::QuotedString(..) => {
                    self.string_quote_tracking.unescaped_quote_count
                }
                _ => 0,
            }
        } else {
            0
        };

        if let Some((_idx, next_char)) = next.peek() {
            match next_char {
                ':' | '}' if in_object_key => {
                    // We're ready to close the key
                    log::debug!("Closing due to: key");
                    true
                }
                ','
                    if (in_object_value || in_array) && closing_char_count % 2 == 0 =>
                {
                    // We're ready to close the value
                    log::debug!("Closing due to: value");
                    true
                }
                '}' if in_object_value => {
                    // We're ready to close the value
                    log::debug!("Closing due to: value");
                    true
                }
                ']' if in_array => {
                    // We're ready to close the value
                    log::debug!("Closing due to: array");
                    true
                }
                ' ' | '\t' | '\n' => {
                    // look ahead and see if we can find a closing bracket or comma
                    while let Some((_, c)) = next.next() {
                        match c {
                            ' ' | '\t' | '\n' => {}
                            '}' if in_object_key || in_object_value => return true,
                            ':' if in_object_key => return true,
                            ',' if in_object_value => return true,
                            ',' | ']' if in_array => return true,
                            '/' => {
                                // Could be a comment
                                if matches!(next.peek(), Some((_, '/' | '*'))) {
                                    // We're ready to close the comment
                                    return true;
                                }
                                return false;
                            }
                            _ => return false,
                        }
                    }
                    // If we faile, terminate the string
                    true
                }
                x if closing_char == *x => {
                    // We'll close the string the next time around.
                    false
                }
                '{' | '"' | '\'' | '[' if !has_some_object => {
                    // We're in a string
                    true
                }
                _ => {
                    // Almost every other character should not close the string
                    false
                }
            }
        } else {
            true
        }
    }

    /// Processes a single character token in the context of the current parser
    /// state. Returns the number of additional characters consumed from `next`
    /// that the caller should skip in the outer iteration loop.
    pub fn process_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        // println!("Processing: {:?}..{:?}", token, next.peek());
        if let Some((last, _)) = self.collection_stack.last() {
            match last {
                JsonCollection::Object(_, _, _) => Ok(self.process_object_token(token, next)),
                JsonCollection::Array(_, _) => Ok(self.process_array_token(token, next)),
                JsonCollection::TripleQuotedString(_, _) => {
                    self.process_triple_quoted_token(token, next)
                }
                JsonCollection::QuotedString(_, _) => self.process_quoted_token(token, next),
                JsonCollection::TripleBacktickString { .. } => {
                    self.process_triple_backtick_token(token, next)
                }
                JsonCollection::BacktickString(_, _) => self.process_backtick_token(token, next),
                JsonCollection::SingleQuotedString(_, _) => {
                    self.process_single_quoted_token(token, next)
                }
                JsonCollection::UnquotedString(_, _) => self.process_unquoted_token(token, next),
                JsonCollection::TrailingComment(_, _) => self.process_trailing_comment(token, next),
                JsonCollection::BlockComment(_, _) => self.process_block_comment(token, next),
            }
        } else {
            // We could be expecting:
            // - A value
            // - Any leading whitespace
            let preview = next.peekable();
            Ok(self.find_any_starting_value(token, preview))
        }
    }

    // -- process_*_token helper methods --

    fn process_object_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> usize {
        match token {
            '}' => {
                self.complete_collection(CompletionState::Complete);
                0
            }
            ',' | ':' => 0,
            _ => self.find_any_starting_value(token, next),
        }
    }

    fn process_array_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> usize {
        match token {
            ']' => {
                self.complete_collection(CompletionState::Complete);
                0
            }
            ',' => 0,
            _ => self.find_any_starting_value(token, next),
        }
    }

    fn process_triple_quoted_token(
        &mut self,
        token: char,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        if token == '"' {
            let is_triple_quoted = match next.peek() {
                Some((_, '"')) => matches!(next.peek(), Some((_, '"')) | None),
                None => true,
                _ => false,
            };
            if is_triple_quoted {
                self.complete_collection(CompletionState::Complete);
                Ok(3)
            } else {
                self.consume(token)
            }
        } else {
            self.consume(token)
        }
    }

    fn process_quoted_token(
        &mut self,
        token: char,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        match token {
            '"' => {
                if self.should_close_string(next, '"') {
                    self.complete_collection(CompletionState::Complete);
                    Ok(0)
                } else {
                    self.consume(token)
                }
            }
            '\\' => match next.peek() {
                Some((_, 'n')) => {
                    self.consume('\n')?;
                    Ok(1)
                }
                Some((_, 't')) => {
                    self.consume('\t')?;
                    Ok(1)
                }
                Some((_, 'r')) => {
                    self.consume('\r')?;
                    Ok(1)
                }
                Some((_, 'b')) => {
                    self.consume('\x08')?;
                    Ok(1)
                }
                Some((_, 'f')) => {
                    self.consume('\x0C')?;
                    Ok(1)
                }
                Some((_, '\\')) => {
                    self.consume('\\')?;
                    Ok(1)
                }
                Some((_, '"')) => {
                    self.consume('"')?;
                    Ok(1)
                }
                Some((_, 'u')) => {
                    let mut buffer = String::new();
                    buffer.push(token);
                    for _ in 0..4 {
                        if let Some((_, c)) = next.next() {
                            buffer.push(c);
                        } else {
                            break;
                        }
                    }
                    for c in buffer.chars() {
                        let _ = self.consume(c);
                    }
                    Ok(5)
                }
                _ => self.consume(token),
            },
            _ => self.consume(token),
        }
    }

    fn process_triple_backtick_token(
        &mut self,
        token: char,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        if token == '`' {
            let is_triple_quoted = next
                .next_if(|&(_, c)| c == '`')
                .and_then(|_| next.next_if(|&(_, c)| c == '`'))
                .is_some();
            if is_triple_quoted {
                self.complete_collection(CompletionState::Complete);
                Ok(2)
            } else {
                self.consume(token)
            }
        } else {
            self.consume(token)
        }
    }

    fn process_backtick_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        match token {
            '`' => {
                if self.should_close_string(next, '`') {
                    self.complete_collection(CompletionState::Complete);
                    Ok(0)
                } else {
                    self.consume(token)
                }
            }
            _ => self.consume(token),
        }
    }

    fn process_single_quoted_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        match token {
            '\'' => {
                if self.should_close_string(next, '\'') {
                    self.complete_collection(CompletionState::Complete);
                    Ok(0)
                } else {
                    self.consume(token)
                }
            }
            _ => self.consume(token),
        }
    }

    fn process_unquoted_token(
        &mut self,
        token: char,
        next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        let res = self.consume(token);
        if let CloseStringResult::Close(count, completion) =
            self.should_close_unescaped_string(next)
        {
            self.complete_collection(completion);
            Ok(count)
        } else {
            res
        }
    }

    fn process_trailing_comment(
        &mut self,
        token: char,
        _next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        match token {
            '\n' => {
                self.complete_collection(CompletionState::Complete);
                Ok(0)
            }
            _ => self.consume(token),
        }
    }

    fn process_block_comment(
        &mut self,
        token: char,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Result<usize, JsonishError> {
        match token {
            '*' => match next.peek() {
                Some((_, '/')) => {
                    self.complete_collection(CompletionState::Complete);
                    Ok(1)
                }
                _ => Ok(0),
            },
            _ => self.consume(token),
        }
    }

    /// Attempts to start parsing a new JSON value from the given token character.
    /// Pushes the appropriate collection type onto the stack and returns the
    /// number of additional characters consumed from `next`.
    fn start_double_quoted_string(
        &mut self,
        next: &mut Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> usize {
        let is_triple_quoted = next
            .next_if(|&(_, c)| c == '"')
            .and_then(|_| next.next_if(|&(_, c)| c == '"'))
            .is_some();

        if is_triple_quoted {
            self.collection_stack.push((
                JsonCollection::TripleQuotedString(String::new(), CompletionState::Incomplete),
                Vec::default(),
            ));
            return 2;
        }
        self.reset_quote_tracking();
        self.collection_stack.push((
            JsonCollection::QuotedString(String::new(), CompletionState::Incomplete),
            Vec::default(),
        ));
        0
    }

    fn start_backtick_string(
        &mut self,
        next: &mut Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> usize {
        let is_triple_quoted = next
            .next_if(|&(_, c)| c == '`')
            .and_then(|_| next.next_if(|&(_, c)| c == '`'))
            .is_some();

        if is_triple_quoted {
            self.collection_stack.push((
                JsonCollection::TripleBacktickString {
                    lang: None,
                    path: None,
                    content: (String::new(), CompletionState::Incomplete),
                },
                Vec::default(),
            ));
            return 2;
        }
        self.collection_stack.push((
            JsonCollection::BacktickString(String::new(), CompletionState::Incomplete),
            Vec::default(),
        ));
        0
    }

    fn start_slash(
        &mut self,
        token: char,
        next: &mut Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> Option<usize> {
        match next.peek() {
            Some((_, '/')) => {
                self.collection_stack.push((
                    JsonCollection::TrailingComment(String::new(), CompletionState::Incomplete),
                    Vec::default(),
                ));
                Some(1)
            }
            Some((_, '*')) => {
                self.collection_stack.push((
                    JsonCollection::BlockComment(String::new(), CompletionState::Incomplete),
                    Vec::default(),
                ));
                Some(1)
            }
            _ => {
                if matches!(
                    self.collection_stack.last(),
                    Some((JsonCollection::Object(_, _, _), _))
                ) {
                    self.collection_stack.push((
                        JsonCollection::UnquotedString(token.into(), CompletionState::Incomplete),
                        Vec::default(),
                    ));
                    return Some(0);
                }
                None
            }
        }
    }

    fn find_any_starting_value(
        &mut self,
        token: char,
        mut next: Peekable<impl Iterator<Item = (usize, char)>>,
    ) -> usize {
        match token {
            '{' => {
                self.collection_stack.push((
                    JsonCollection::Object(vec![], vec![], CompletionState::Incomplete),
                    Vec::default(),
                ));
            }
            '[' => {
                self.collection_stack.push((
                    JsonCollection::Array(vec![], CompletionState::Incomplete),
                    Vec::default(),
                ));
            }
            '"' => {
                let count = self.start_double_quoted_string(&mut next);
                if count > 0 {
                    return count;
                }
            }
            '\'' => {
                self.collection_stack.push((
                    JsonCollection::SingleQuotedString(String::new(), CompletionState::Incomplete),
                    Vec::default(),
                ));
            }
            '`' => {
                let count = self.start_backtick_string(&mut next);
                if count > 0 {
                    return count;
                }
            }
            '/' => {
                if let Some(count) = self.start_slash(token, &mut next) {
                    return count;
                }
            }
            x if x.is_whitespace() => {}
            x => {
                self.collection_stack.push((
                    JsonCollection::UnquotedString(x.into(), CompletionState::Incomplete),
                    Vec::default(),
                ));
                if let CloseStringResult::Close(count, completion) =
                    self.should_close_unescaped_string(next)
                {
                    self.complete_collection(completion);
                    return count;
                }
            }
        }

        0
    }
}

/// Result of `should_close_unescaped_string`: either close the string with a
/// character count and completion state, or continue accumulating characters.
#[derive(Debug, PartialEq)]
enum CloseStringResult {
    Close(usize, CompletionState),
    Continue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_action::jsonish::CompletionState;

    /// Test the InObjectValue branch of should_close_unescaped_string directly.
    ///
    /// The off-by-one bug on this branch only manifests during streaming (multiple
    /// parse() calls on successive chunks), not in single-pass parsing. Testing
    /// through the public parse() API cannot catch it because the re-processed
    /// character gets absorbed harmlessly. So we test the private function directly
    /// by setting up the collection stack to simulate being inside an object value.
    #[test]
    fn test_should_close_unescaped_string_in_object_value_exhausted() {
        let mut state = JsonParseState::new();
        // Set up stack: Object with one key but no value yet (InObjectValue),
        // then an UnquotedString being accumulated on top.
        state.collection_stack.push((
            JsonCollection::Object(vec!["key".to_string()], vec![], CompletionState::Incomplete),
            Vec::default(),
        ));
        state.collection_stack.push((
            JsonCollection::UnquotedString("hello".to_string(), CompletionState::Incomplete),
            Vec::default(),
        ));

        // Remaining chars: "world" — no ',' or '}' to trigger Complete
        let remaining: Vec<(usize, char)> = vec![(0, 'w'), (1, 'o'), (2, 'r'), (3, 'l'), (4, 'd')];
        let result = state.should_close_unescaped_string(remaining.into_iter().peekable());

        // counter should be 5 (last idx=4, +1), not 4
        assert_eq!(
            result,
            CloseStringResult::Close(5, CompletionState::Incomplete)
        );
    }
}
