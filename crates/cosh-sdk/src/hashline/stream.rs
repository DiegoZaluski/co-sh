//! Lazily format a stream of file lines into hashline-numbered chunks, yielded
//! as bounded text chunks. Used to send `read`-style file content to consumers
//! without materializing the full file at once.
//!
//! Each yielded chunk is at most [`StreamOptions::max_chunk_lines`] lines and
//! at most [`StreamOptions::max_chunk_bytes`] UTF-8 bytes (whichever fires
//! first).
use super::format::format_numbered_line;
use super::types::StreamOptions;

struct ResolvedStreamOptions {
    start_line: u32,
    max_chunk_lines: u32,
    max_chunk_bytes: u32,
}

impl From<StreamOptions> for ResolvedStreamOptions {
    fn from(opts: StreamOptions) -> Self {
        Self {
            start_line: opts.start_line.unwrap_or(1),
            max_chunk_lines: opts.max_chunk_lines.unwrap_or(200),
            max_chunk_bytes: opts.max_chunk_bytes.unwrap_or(64 * 1024),
        }
    }
}

struct ChunkEmitter {
    line_number: u32,
    out_lines: Vec<String>,
    out_bytes: usize,
    max_chunk_lines: u32,
    max_chunk_bytes: u32,
}

impl ChunkEmitter {
    const fn new(options: &ResolvedStreamOptions) -> Self {
        Self {
            line_number: options.start_line,
            out_lines: Vec::new(),
            out_bytes: 0,
            max_chunk_lines: options.max_chunk_lines,
            max_chunk_bytes: options.max_chunk_bytes,
        }
    }

    fn push_line(&mut self, line: &str) -> Vec<String> {
        let formatted = format_numbered_line(self.line_number, line); // (e.g. "1:line content")
        self.line_number += 1;

        let mut chunks: Vec<String> = Vec::new();
        let sep_bytes = usize::from(!self.out_lines.is_empty());
        let line_bytes = formatted.len();

        let would_overflow = u32::try_from(self.out_lines.len()).unwrap_or(u32::MAX)
            >= self.max_chunk_lines
            || self.out_bytes + sep_bytes + line_bytes > self.max_chunk_bytes as usize;

        if !self.out_lines.is_empty()
            && would_overflow
            && let Some(flushed) = self.flush()
        {
            chunks.push(flushed);
        }

        let is_first = self.out_lines.is_empty();
        self.out_lines.push(formatted);

        self.out_bytes += usize::from(!is_first) + line_bytes;

        if (u32::try_from(self.out_lines.len()).unwrap_or(u32::MAX) >= self.max_chunk_lines
            || self.out_bytes >= self.max_chunk_bytes as usize)
            && let Some(flushed) = self.flush()
        {
            chunks.push(flushed);
        }

        chunks
    }

    /// Flush the current buffer and return the chunk, or `None` if empty.
    fn flush(&mut self) -> Option<String> {
        if self.out_lines.is_empty() {
            return None;
        }
        let chunk = self.out_lines.join("\n");
        self.out_lines.clear();
        self.out_bytes = 0;
        Some(chunk)
    }
}

/// Format a text source into hashline-numbered line chunks.
///
/// Each yielded chunk respects [`StreamOptions::max_chunk_lines`] and
/// [`StreamOptions::max_chunk_bytes`] (whichever fires first).
///
/// When the source is empty or contains only blank lines, a single chunk
/// with a numbered empty line is yielded so the consumer always sees at
/// least one numbered line.
#[must_use]
pub fn stream_hash_lines(source: &str, options: StreamOptions) -> Vec<String> {
    let resolved = ResolvedStreamOptions::from(options);
    let mut emitter = ChunkEmitter::new(&resolved);

    let mut chunks: Vec<String> = Vec::new();
    let mut saw_any_line = false;
    let mut remaining = source;

    // Process complete lines delimited by '\n' — mirrors the TS for-await
    // loop that feeds lines to the emitter as they arrive.
    while let Some(nl) = remaining.find('\n') {
        let (head, tail) = (&remaining[..nl], &remaining[nl + 1..]);
        let line = head.strip_suffix('\r').unwrap_or(head);
        saw_any_line = true;
        chunks.extend(emitter.push_line(line));
        remaining = &tail[1..]; // skip '\n'
    }

    // Remaining content after the last '\n' (the TS TextDecoder flush path).
    // If the source ends with '\n', remaining is "" and we skip it — no
    // phantom blank line for a trailing newline.
    if !remaining.is_empty() {
        let tail = remaining.strip_suffix('\r').unwrap_or(remaining);
        saw_any_line = true;
        chunks.extend(emitter.push_line(tail));
    }

    // When the source is empty (no bytes at all), emit one numbered blank
    // line so the consumer always sees at least one line.
    if !saw_any_line {
        chunks.extend(emitter.push_line(""));
    }

    if let Some(last) = emitter.flush() {
        chunks.push(last);
    }

    chunks
}
