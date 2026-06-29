use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::rgba::{
    DEFAULT_BACKGROUND_RGB, DEFAULT_FOREGROUND_RGB, RGBA, ansi256_index_to_rgb,
};

/// Hex color string or null (None = unknown)
pub type HexColor = Option<String>;

// Not ported: env.ts (registerEnvVar/OTUI_PALETTE_IDLE_TIMEOUT_MS).
// Rust uses std::env::var() directly instead of a JS env registry.
fn palette_idle_timeout_ms() -> u64 {
    std::env::var("OTUI_PALETTE_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

fn is_tty() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Signals a reader thread to stop. Dropped automatically when the caller exits scope.
struct ReaderGuard {
    stop: Arc<AtomicBool>,
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct TerminalColors {
    pub palette: Vec<HexColor>,
    pub default_foreground: HexColor,
    pub default_background: HexColor,
    pub cursor_color: HexColor,
    pub mouse_foreground: HexColor,
    pub mouse_background: HexColor,
    pub tek_foreground: HexColor,
    pub tek_background: HexColor,
    pub highlight_background: HexColor,
    pub highlight_foreground: HexColor,
}

#[derive(Debug, Clone, Copy)]
pub struct GetPaletteOptions {
    pub timeout_ms: u64,
    pub size: u8,
}

impl Default for GetPaletteOptions {
    fn default() -> Self {
        GetPaletteOptions {
            timeout_ms: 5000,
            size: 16,
        }
    }
}

pub trait TerminalPaletteDetector {
    /// Detect the terminal color palette.
    ///
    /// # Errors
    /// Returns an error if writing the OSC query sequence fails.
    fn detect(&mut self, options: Option<&GetPaletteOptions>) -> Result<TerminalColors, String>;
    /// Detect whether the terminal supports OSC color queries.
    ///
    /// # Errors
    /// Returns an error if writing the OSC query sequence fails.
    fn detect_osc_support(&mut self, timeout_ms: u64) -> Result<bool, String>;
    fn cleanup(&mut self);
}

#[derive(Debug, Clone)]
pub struct NormalizedTerminalPalette {
    pub palette: Vec<RGBA>,
    pub default_foreground: RGBA,
    pub default_background: RGBA,
}

pub type WriteFunction = Box<dyn Fn(&str) -> io::Result<()> + Send>;

pub type OscSubscriptionSource = Box<dyn Fn(Box<dyn Fn(&str) + Send>) -> Box<dyn FnOnce() + Send>>;

#[derive(Default)]
pub struct TerminalPaletteOptions {
    pub write_fn: Option<WriteFunction>,
    pub is_legacy_tmux: bool,
    pub is_tmux: bool,
    pub osc_source: Option<OscSubscriptionSource>,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn scale_component(comp: &str) -> String {
    let val = u16::from_str_radix(comp, 16).unwrap_or(0);
    let max_in = (1u64 << (4 * comp.len())) - 1;
    let scaled = ((f64::from(val) / max_in as f64) * 255.0).round() as u8;
    format!("{scaled:02x}")
}

fn to_hex(r: Option<&str>, g: Option<&str>, b: Option<&str>, hex6: Option<&str>) -> String {
    if let Some(h) = hex6 {
        return format!("#{hex}", hex = h.to_lowercase());
    }
    if let (Some(r), Some(g), Some(b)) = (r, g, b) {
        return format!(
            "#{}{}{}",
            scale_component(r),
            scale_component(g),
            scale_component(b)
        );
    }
    "#000000".to_string()
}

/// Wrap OSC sequence for tmux passthrough.
/// tmux requires DCS sequences to pass OSC to the underlying terminal.
/// Format: `ESC P tmux; ESC <OSC_SEQUENCE> ESC \`
fn wrap_for_tmux(osc: &str) -> String {
    let escaped = osc.replace('\x1b', "\x1b\x1b");
    format!("\x1bPtmux;{escaped}\x1b\\")
}

fn parse_osc4_response(data: &str) -> Vec<(u8, String)> {
    let mut results = Vec::new();
    let mut remaining = data;

    while let Some(pos) = remaining.find("\x1b]4;") {
        remaining = &remaining[(pos + 4)..];

        let Some(semi) = remaining.find(';') else {
            break;
        };
        let Ok(index) = remaining[..semi].parse::<u8>() else {
            remaining = &remaining[(semi + 1)..];
            continue;
        };
        remaining = &remaining[(semi + 1)..];

        let Some((color_str, rest)) = consume_osc_value(remaining) else {
            break;
        };

        let color_str = color_str.trim();

        if let Some(hex6) = color_str.strip_prefix('#') {
            let hex6: String = hex6.chars().filter(char::is_ascii_hexdigit).collect();
            if hex6.len() >= 6 {
                results.push((index, format!("#{hex}", hex = hex6[..6].to_lowercase())));
            }
        } else if let Some(rgb_str) = color_str.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb_str.split('/').collect();
            if parts.len() >= 3 {
                results.push((
                    index,
                    to_hex(Some(parts[0]), Some(parts[1]), Some(parts[2]), None),
                ));
            }
        }

        remaining = rest;
    }

    results
}

fn parse_osc_special_response(data: &str) -> Vec<(u8, String)> {
    let mut results = Vec::new();
    let mut remaining = data;

    while let Some(pos) = remaining.find("\x1b]") {
        remaining = &remaining[(pos + 2)..];

        let Some(semi) = remaining.find(';') else {
            break;
        };
        let Ok(index) = remaining[..semi].parse::<u8>() else {
            remaining = &remaining[(semi + 1)..];
            continue;
        };
        remaining = &remaining[(semi + 1)..];

        let Some((color_str, rest)) = consume_osc_value(remaining) else {
            break;
        };

        let color_str = color_str.trim();

        if let Some(hex6) = color_str.strip_prefix('#') {
            let hex6: String = hex6.chars().filter(char::is_ascii_hexdigit).collect();
            if hex6.len() >= 6 {
                results.push((index, format!("#{hex}", hex = hex6[..6].to_lowercase())));
            }
        } else if let Some(rgb_str) = color_str.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb_str.split('/').collect();
            if parts.len() >= 3 {
                results.push((
                    index,
                    to_hex(Some(parts[0]), Some(parts[1]), Some(parts[2]), None),
                ));
            }
        }

        remaining = rest;
    }

    results
}

/// Consume an OSC value terminated by BEL (\x07) or ST (\x1b\\)
fn consume_osc_value(data: &str) -> Option<(&str, &str)> {
    let bytes = data.as_bytes();
    for pos in 0..bytes.len() {
        if bytes[pos] == 0x07 {
            return Some((&data[..pos], &data[pos + 1..]));
        }
        if bytes[pos] == 0x1b {
            if pos + 1 < bytes.len() && bytes[pos + 1] == b'\\' {
                return Some((&data[..pos], &data[pos + 2..]));
            }
            return Some((&data[..pos], &data[pos..]));
        }
    }
    None
}

fn get_fallback_ansi256_palette() -> &'static [RGBA; 256] {
    static PALETTE: LazyLock<[RGBA; 256]> = LazyLock::new(|| {
        let mut palette = [RGBA::from_ints(0, 0, 0, 255); 256];
        for i in 0..256u16 {
            #[allow(clippy::cast_possible_truncation)]
            let (r, g, b) = ansi256_index_to_rgb(i as u8);
            palette[i as usize] = RGBA::from_ints(r, g, b, 255);
        }
        palette
    });
    &PALETTE
}

#[allow(clippy::similar_names)]
#[must_use]
pub fn normalize_terminal_palette(colors: Option<&TerminalColors>) -> NormalizedTerminalPalette {
    let fallback_palette = get_fallback_ansi256_palette();

    let default_fg_rgba = RGBA::from_ints(
        DEFAULT_FOREGROUND_RGB.0,
        DEFAULT_FOREGROUND_RGB.1,
        DEFAULT_FOREGROUND_RGB.2,
        255,
    );
    let default_bg_rgba = RGBA::from_ints(
        DEFAULT_BACKGROUND_RGB.0,
        DEFAULT_BACKGROUND_RGB.1,
        DEFAULT_BACKGROUND_RGB.2,
        255,
    );

    let palette: Vec<RGBA> = (0..256)
        .map(|i| {
            if let Some(colors) = colors
                && let Some(Some(detected)) = colors.palette.get(i)
            {
                return RGBA::from_hex(detected);
            }
            fallback_palette[i]
        })
        .collect();
    let default_foreground = colors
        .and_then(|c| c.default_foreground.as_ref())
        .map_or(default_fg_rgba, |h| RGBA::from_hex(h));

    let default_background = colors
        .and_then(|c| c.default_background.as_ref())
        .map_or(default_bg_rgba, |h| RGBA::from_hex(h));

    NormalizedTerminalPalette {
        palette,
        default_foreground,
        default_background,
    }
}

#[must_use]
pub fn build_terminal_palette_signature(colors: Option<&TerminalColors>) -> String {
    let normalized = normalize_terminal_palette(colors);
    let palette_sig: Vec<String> = normalized
        .palette
        .iter()
        .map(|c| {
            let (r, g, b, _a) = c.to_ints();
            format!("{r},{g},{b}")
        })
        .collect();
    let palette_sig = palette_sig.join(";");
    let (fg_r, fg_g, fg_b, _) = normalized.default_foreground.to_ints();
    let (bg_r, bg_g, bg_b, _) = normalized.default_background.to_ints();
    format!("{palette_sig}|{fg_r},{fg_g},{fg_b}|{bg_r},{bg_g},{bg_b}")
}

pub struct TerminalPalette {
    write_fn: Option<WriteFunction>,
    in_legacy_tmux: bool,
    in_tmux: bool,
    _osc_source: Option<OscSubscriptionSource>,
}

impl TerminalPalette {
    #[must_use]
    pub fn new(options: TerminalPaletteOptions) -> Self {
        let in_legacy_tmux = options.is_legacy_tmux;
        TerminalPalette {
            write_fn: options.write_fn,
            in_legacy_tmux,
            in_tmux: options.is_tmux || in_legacy_tmux,
            _osc_source: options.osc_source,
        }
    }

    fn write_osc(&self, osc: &str, wrap_for_legacy_tmux: bool) -> io::Result<()> {
        let data = if wrap_for_legacy_tmux && self.in_legacy_tmux {
            wrap_for_tmux(osc)
        } else {
            osc.to_string()
        };

        if let Some(ref write_fn) = self.write_fn {
            write_fn(&data)
        } else {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(data.as_bytes())?;
            handle.flush()
        }
    }

    /// Spawn a background reader thread. The caller must keep `ReaderGuard` alive
    /// for the duration of the session; dropping it signals the thread to stop.
    fn spawn_reader(stop: Arc<AtomicBool>) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let mut stdin = io::stdin();
            let mut buf = [0u8; 1024];
            while !stop.load(Ordering::Relaxed) {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if tx.send(chunk).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        rx
    }

    /// Detect whether the terminal supports OSC color queries.
    ///
    /// # Errors
    /// Returns an error if writing the OSC query sequence fails.
    pub fn detect_osc_support(&mut self, timeout_ms: u64) -> Result<bool, String> {
        if !is_tty() {
            return Ok(false);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let _guard = ReaderGuard { stop: stop.clone() };
        let rx = Self::spawn_reader(stop);

        self.write_osc("\x1b]4;0;?\x07", true)
            .map_err(|e| format!("Failed to write OSC query: {e}"))?;

        let mut buffer = String::new();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }

            match rx.recv_timeout(remaining) {
                Ok(chunk) => {
                    buffer.push_str(&chunk);
                    if buffer.len() > 8192 {
                        buffer = buffer[buffer.len().saturating_sub(4096)..].to_string();
                    }
                    let parsed = parse_osc4_response(&buffer);
                    if !parsed.is_empty() {
                        return Ok(true);
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    return Ok(false);
                }
            }
        }
    }

    fn query_palette(
        &mut self,
        indices: &[u8],
        timeout_ms: u64,
        idle_timeout_ms: u64,
    ) -> Result<HashMap<u8, HexColor>, String> {
        if !is_tty() {
            let mut results: HashMap<u8, HexColor> = HashMap::new();
            for &i in indices {
                results.insert(i, None);
            }
            return Ok(results);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let _guard = ReaderGuard { stop: stop.clone() };
        let rx = Self::spawn_reader(stop);

        let mut results: HashMap<u8, HexColor> = HashMap::new();
        for &i in indices {
            results.insert(i, None);
        }

        let queries = indices.iter().fold(String::new(), |mut s, i| {
            let _ = write!(s, "\x1b]4;{i};?\x07");
            s
        });
        self.write_osc(&queries, true)
            .map_err(|e| format!("Failed to write palette query: {e}"))?;

        let mut buffer = String::new();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            // Use whichever fires sooner: overall deadline or idle timeout
            let idle_deadline = now + Duration::from_millis(idle_timeout_ms);
            let wait = std::cmp::min(
                deadline.saturating_duration_since(now),
                idle_deadline.saturating_duration_since(Instant::now()),
            );

            match rx.recv_timeout(wait) {
                Ok(chunk) => {
                    buffer.push_str(&chunk);
                    if buffer.len() > 8192 {
                        buffer = buffer[buffer.len().saturating_sub(4096)..].to_string();
                    }
                    let parsed = parse_osc4_response(&buffer);
                    for (idx, hex) in &parsed {
                        if results.contains_key(idx) {
                            results.insert(*idx, Some(hex.clone()));
                        }
                    }

                    let done_count = results.values().filter(|v| v.is_some()).count();
                    if done_count == results.len() {
                        break;
                    }
                    // Idle deadline resets on data — continue loop
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        Ok(results)
    }

    fn query_special_colors(
        &mut self,
        timeout_ms: u64,
        idle_timeout_ms: u64,
    ) -> Result<HashMap<u8, HexColor>, String> {
        if !is_tty() {
            let mut results: HashMap<u8, HexColor> = HashMap::new();
            let fallback = [10, 11, 12, 13, 14, 15, 16, 17, 19];
            for &i in &fallback {
                results.insert(i, None);
            }
            return Ok(results);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let _guard = ReaderGuard { stop: stop.clone() };
        let rx = Self::spawn_reader(stop);

        let mut results: HashMap<u8, HexColor> = HashMap::new();
        let all_queries: [u8; 9] = [10, 11, 12, 13, 14, 15, 16, 17, 19];

        // tmux handles plain OSC 10/11/12 only; it has no OSC 13-17/19 handlers or reply routing.
        let queries: &[u8] = if self.in_tmux {
            &[10, 11, 12]
        } else {
            &all_queries
        };

        for &i in queries {
            results.insert(i, None);
        }

        let query_str = queries.iter().fold(String::new(), |mut s, i| {
            let _ = write!(s, "\x1b]{i};\x07");
            s
        });
        self.write_osc(&query_str, false)
            .map_err(|e| format!("Failed to write special color query: {e}"))?;

        let mut buffer = String::new();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let idle_deadline = now + Duration::from_millis(idle_timeout_ms);
            let wait = std::cmp::min(
                deadline.saturating_duration_since(now),
                idle_deadline.saturating_duration_since(Instant::now()),
            );

            match rx.recv_timeout(wait) {
                Ok(chunk) => {
                    buffer.push_str(&chunk);
                    if buffer.len() > 8192 {
                        buffer = buffer[buffer.len().saturating_sub(4096)..].to_string();
                    }
                    let parsed = parse_osc_special_response(&buffer);
                    for (idx, hex) in &parsed {
                        if results.contains_key(idx) {
                            results.insert(*idx, Some(hex.clone()));
                        }
                    }

                    let all_done = queries
                        .iter()
                        .all(|i| results.get(i).is_some_and(Option::is_some));
                    if all_done {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }

        Ok(results)
    }

    /// Detect the terminal color palette.
    ///
    /// # Errors
    /// Returns an error if writing the OSC query sequence fails.
    pub fn detect(
        &mut self,
        options: Option<&GetPaletteOptions>,
    ) -> Result<TerminalColors, String> {
        let opts = options.copied().unwrap_or_default();
        let timeout = opts.timeout_ms;
        let size = opts.size;

        let supported = self.detect_osc_support(timeout)?;

        if !supported {
            return Ok(TerminalColors {
                palette: vec![None; size as usize],
                default_foreground: None,
                default_background: None,
                cursor_color: None,
                mouse_foreground: None,
                mouse_background: None,
                tek_foreground: None,
                tek_background: None,
                highlight_background: None,
                highlight_foreground: None,
            });
        }

        let indices: Vec<u8> = (0..size).collect();
        let idle_tout = palette_idle_timeout_ms();
        let pal_results = self.query_palette(&indices, timeout, idle_tout)?;
        let spc_colors = self.query_special_colors(timeout, idle_tout)?;

        let palette: Vec<HexColor> = (0..size)
            .map(|i| pal_results.get(&i).and_then(Clone::clone))
            .collect();

        Ok(TerminalColors {
            palette,
            default_foreground: spc_colors.get(&10).and_then(Clone::clone),
            default_background: spc_colors.get(&11).and_then(Clone::clone),
            cursor_color: spc_colors.get(&12).and_then(Clone::clone),
            mouse_foreground: spc_colors.get(&13).and_then(Clone::clone),
            mouse_background: spc_colors.get(&14).and_then(Clone::clone),
            tek_foreground: spc_colors.get(&15).and_then(Clone::clone),
            tek_background: spc_colors.get(&16).and_then(Clone::clone),
            highlight_background: spc_colors.get(&17).and_then(Clone::clone),
            highlight_foreground: spc_colors.get(&19).and_then(Clone::clone),
        })
    }

    pub fn cleanup(&mut self) {}
}

impl TerminalPaletteDetector for TerminalPalette {
    fn detect(&mut self, options: Option<&GetPaletteOptions>) -> Result<TerminalColors, String> {
        self.detect(options)
    }

    fn detect_osc_support(&mut self, timeout_ms: u64) -> Result<bool, String> {
        self.detect_osc_support(timeout_ms)
    }

    fn cleanup(&mut self) {
        self.cleanup();
    }
}

#[must_use]
pub fn create_terminal_palette(options: TerminalPaletteOptions) -> TerminalPalette {
    TerminalPalette::new(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osc4_response_hex6() {
        let data = "\x1b]4;0;#ff0000\x07extra";
        let results = parse_osc4_response(data);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], (0, "#ff0000".to_string()));
    }

    #[test]
    fn test_parse_osc4_response_rgb() {
        let data = "\x1b]4;1;rgb:ff/00/00\x07";
        let results = parse_osc4_response(data);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], (1, "#ff0000".to_string()));
    }

    #[test]
    fn test_parse_osc_special_response() {
        let data = "\x1b]10;rgb:ff/ff/ff\x07\x1b]11;rgb:00/00/00\x07";
        let results = parse_osc_special_response(data);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (10, "#ffffff".to_string()));
        assert_eq!(results[1], (11, "#000000".to_string()));
    }

    #[test]
    fn test_wrap_for_tmux() {
        let result = wrap_for_tmux("\x1b]4;0;?\x07");
        assert_eq!(result, "\x1bPtmux;\x1b\x1b]4;0;?\x07\x1b\\");
    }

    #[test]
    fn test_scale_component() {
        assert_eq!(scale_component("ff"), "ff");
        assert_eq!(scale_component("0"), "00");
        assert_eq!(scale_component("fff"), "ff");
    }

    #[test]
    fn test_normalize_terminal_palette_with_colors() {
        let mut palette = vec![None; 256];
        palette[0] = Some("#ff0000".to_string());
        palette[1] = Some("#00ff00".to_string());
        let colors = TerminalColors {
            palette,
            default_foreground: Some("#ffffff".to_string()),
            default_background: Some("#000000".to_string()),
            cursor_color: None,
            mouse_foreground: None,
            mouse_background: None,
            tek_foreground: None,
            tek_background: None,
            highlight_background: None,
            highlight_foreground: None,
        };

        let normalized = normalize_terminal_palette(Some(&colors));
        assert_eq!(normalized.palette.len(), 256);
        assert_eq!(normalized.palette[0].to_ints(), (255, 0, 0, 255));
        assert_eq!(normalized.palette[1].to_ints(), (0, 255, 0, 255));
        assert_eq!(
            normalized.default_foreground.to_ints(),
            (255, 255, 255, 255)
        );
        assert_eq!(normalized.default_background.to_ints(), (0, 0, 0, 255));
    }

    #[test]
    fn test_build_terminal_palette_signature() {
        let colors = TerminalColors {
            palette: vec![None; 256],
            default_foreground: Some("#ffffff".to_string()),
            default_background: Some("#000000".to_string()),
            cursor_color: None,
            mouse_foreground: None,
            mouse_background: None,
            tek_foreground: None,
            tek_background: None,
            highlight_background: None,
            highlight_foreground: None,
        };
        let sig = build_terminal_palette_signature(Some(&colors));
        assert!(sig.contains("|255,255,255|"));
        assert!(sig.contains("0,0,0"));
    }

    #[test]
    fn test_is_tty_returns_false_in_test() {
        // In a test environment stdin/stdout are not TTYs
        assert!(!is_tty());
    }

    #[test]
    fn test_detect_osc_support_returns_false_when_not_tty() {
        let mut palette = TerminalPalette::new(TerminalPaletteOptions::default());
        let result = palette.detect_osc_support(100).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_detect_returns_fallback_when_not_tty() {
        let mut palette = TerminalPalette::new(TerminalPaletteOptions::default());
        let colors = palette.detect(None).unwrap();
        assert_eq!(colors.palette.len(), 16);
        assert!(colors.default_foreground.is_none());
        assert!(colors.default_background.is_none());
    }
}
