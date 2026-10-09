use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use cosh_tui::core::types::MouseEvent;

use crate::component::wall;
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;
use crate::util::field_selection::DragSelection;

// Content definitions

/// Content for a specific free gateway recommendation.
///
/// Each gateway defines its own constant of this type. The UI dialog is
/// completely generic — it only reads these fields.
pub struct GatewayRecommendationContent {
    /// Short tab label (question-dialog tab bar).
    pub tab_label: &'static str,
    /// Bold question displayed in the offer phase (e.g. "Use the free
    /// Charm gateway with OAuth?").
    pub title: &'static str,
    /// Markdown text revealed progressively during the fake streaming phase.
    /// Rendered via `MarkdownRenderable` as it grows, simulating an agent
    /// response. SHARED across tabs (see `GATEWAY_PITCH`): the streamed
    /// body is one generic message that covers every offered gateway, so a
    /// mid-stream tab switch never rewinds or restarts the reveal.
    pub stream_text: &'static str,
}

/// One selectable gateway in the tabbed offer: the provider id the sign-in
/// dispatches on (the same id the provider registry resolves) plus that
/// gateway's copy. The slice order IS the tab order.
pub struct GatewayOffer {
    /// Provider id (`"charm"`, `"copilot"`).
    pub provider: &'static str,
    pub content: &'static GatewayRecommendationContent,
}

/// Placeholder for future free-gateway recommendations.
///
/// The dialog itself is generic — to recommend a new gateway, define a
/// `GatewayRecommendationContent` const here, add a `GatewayOffer` entry
/// to [`OFFERS`], and route its device sign-in in
/// `gateway_sign_in_inner` (`app/gateway_recommendation.rs`) — the
/// provider id in `OFFERS` is what the dispatcher matches on.
///
/// Example:
/// ```ignore
/// pub const EXAMPLE: GatewayRecommendationContent = GatewayRecommendationContent {
///     tab_label: "Example",
///     title: "Use the Example free gateway?",
///     stream_text: "Description shown while streaming…",
/// };
/// ```
pub const PLACEHOLDER: GatewayRecommendationContent = GatewayRecommendationContent {
    tab_label: "",
    title: "",
    stream_text: "",
};

/// The streamed body, shared by every offered gateway. Generic on purpose:
/// the tabs (not the prose) carry which gateway is which, so one reveal
/// covers Charm and GitHub alike and switching tabs mid-stream is seamless.
const GATEWAY_PITCH: &str = "No provider key is configured, so this model cannot be reached. \
Two gateways are offered as alternatives — select a tab and choose Yes to sign in.\n\n\
**Charm** is free and ready to use. **GitHub Copilot** is available if you already have a \
Copilot subscription. These are recommendations only: cosh has no sponsorship, affiliation \
or agreement with either provider.\n\n\
Choosing Yes opens your browser to authorize cosh with a device code. The token is stored \
in your OS keyring. Choosing No closes this dialog; \u{2190} \u{2192} switch tabs to \
compare the gateways.";

/// Charm Hyper — the free gateway cosh recommends.
///
/// The recommendation is editorial, not sponsored: Charm is recommended
/// because it is good, practical, fairly priced for what it offers and,
/// above all, honest about its contracts — the device protocol cosh speaks
/// is the same one Charm's own CLI uses, in the open, with no surprise
/// deprecations or silent contract changes.
pub const CHARM: GatewayRecommendationContent = GatewayRecommendationContent {
    tab_label: "Charm",
    title: "Sign in to the Charm gateway with OAuth?",
    stream_text: GATEWAY_PITCH,
};

/// GitHub Copilot — for users who already carry a Copilot subscription.
/// The device flow is GitHub's standard RFC 8628 endpoints (see the SDK's
/// `copilot::oauth`), and the resulting token is the `GITHUB_TOKEN`
/// credential the Copilot family resolves.
pub const GITHUB: GatewayRecommendationContent = GatewayRecommendationContent {
    tab_label: "GitHub",
    title: "Sign in to GitHub Copilot with OAuth?",
    stream_text: GATEWAY_PITCH,
};

/// The offered gateways, in tab order. One list drives the whole offer:
/// the dialog renders a tab per entry, and the sign-in dispatch reads the
/// active entry's `provider`.
pub const OFFERS: &[GatewayOffer] = &[
    GatewayOffer {
        provider: "charm",
        content: &CHARM,
    },
    GatewayOffer {
        provider: "copilot",
        content: &GITHUB,
    },
];

/// Characters revealed per second during the fake streaming phase.
const CHARS_PER_SECOND: f64 = 120.0;

/// Minimum height in rows (1 padding top + 1 padding bottom).
const MIN_HEIGHT: u16 = 2;

// Streaming phase

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Text is being revealed character by character.
    Streaming,
    /// Full text shown, waiting for user to pick Yes/No.
    WaitingForResponse,
    /// Opt-in accepted, the device flow is starting: the box STAYS OPEN
    /// (no hide/reopen flicker) until `Started` moves it to Authorizing.
    Connecting,
    /// Opt-in accepted and the device code arrived: the Yes/No options gave
    /// way to the device sign-in display — device code, verification URL and
    /// a copy hint. Everything on it is drag-selectable; the box stays up
    /// until the flow ends.
    Authorizing,
}

/// Which kind of row a copy-document row is: soft-wrapped body rows re-join
/// with spaces on copy (they are one logical paragraph), hard rows (title,
/// device code, URL) are lines of their own and join with newlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyRowKind {
    Body,
    Hard,
}

// Dialog

/// Generic inline dialog that recommends a free gateway.
///
/// Displays a fake streaming agent message (markdown text revealed
/// progressively), followed by a Yes/No question styled like the
/// permission dialog. Rendered inline between chat messages and the
/// prompt, mutually exclusive with `PermissionDialog`, `QuestionDialog`,
/// and `QueueChoiceDialog`.
pub struct FreeGatewayRecommendationDialog {
    pub visible: bool,
    /// `0` = Yes, `1` = No.
    pub selected: usize,
    /// Set to `true` when the user presses Enter or clicks an option.
    pub submitted: bool,
    /// The ACTIVE tab's content (varies per gateway; the only content the
    /// render/copy paths read).
    content: &'static GatewayRecommendationContent,
    /// The offered gateways, in tab order. Empty = single-content mode (no
    /// tab bar, generic/tests use): Left/Right keep driving Yes/No. Non-empty
    /// = tabbed mode: Left/Right switch tabs, Up/Down drive Yes/No — the
    /// arrows' two jobs never share a key.
    offers: &'static [GatewayOffer],
    /// Which offer tab is active (index into `offers`).
    active_tab: usize,
    /// Current streaming phase.
    phase: Phase,
    /// How many *characters* of `stream_text` have been revealed so far.
    /// Tracked as a char index (not byte offset) so slicing is always on a
    /// valid char boundary even with multi-byte UTF-8 like `—` or `–`.
    stream_chars: usize,
    /// Wall-clock timestamp of the last `advance_stream()` call.
    last_frame: Instant,
    /// Live device sign-in shown in the `Authorizing` phase:
    /// (verification_url, user_code). `None` outside that phase.
    sign_in: Option<(String, String)>,
    /// Mouse drag selection over the box's copy document — ONE flat,
    /// char-addressed text made of every copyable row in visual order (see
    /// `copy_doc`). A single range means a drag emends across rows: body →
    /// question, or body → device code, is one selection, one copy.
    selection: Option<DragSelection<()>>,
    /// Wrap width captured when the selection anchored, so a mid-drag
    /// resize cannot silently point the offsets at other characters.
    sel_width: u16,
}

impl FreeGatewayRecommendationDialog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            submitted: false,
            content: &PLACEHOLDER, // placeholder, overwritten by `show*()`
            offers: &[],
            active_tab: 0,
            phase: Phase::Streaming,
            stream_chars: 0,
            last_frame: Instant::now(),
            sign_in: None,
            selection: None,
            sel_width: 0,
        }
    }

    /// Open the dialog for a single recommendation (no tab bar).
    pub fn show(&mut self, content: &'static GatewayRecommendationContent) {
        self.offers = &[];
        self.active_tab = 0;
        self.content = content;
        self.reset_for_show();
    }

    /// Open the dialog with one tab per offered gateway (question-dialog
    /// tab-bar design). The streamed body is shared across tabs, so the
    /// reveal never rewinds on a switch.
    pub fn show_offers(&mut self, offers: &'static [GatewayOffer]) {
        self.offers = offers;
        self.active_tab = 0;
        self.content = offers.first().map_or(&PLACEHOLDER, |o| o.content);
        self.reset_for_show();
    }

    /// State shared by both open paths: a fresh offer, nothing in flight.
    fn reset_for_show(&mut self) {
        self.visible = true;
        self.submitted = false;
        self.selected = 0;
        self.phase = Phase::Streaming;
        self.stream_chars = 0;
        self.last_frame = Instant::now();
        self.sign_in = None;
        self.selection = None;
        self.sel_width = 0;
    }

    /// Number of tabs the tab bar renders (1 = no bar).
    fn tab_count(&self) -> usize {
        if self.offers.is_empty() {
            1
        } else {
            self.offers.len()
        }
    }

    /// Whether the tab bar is drawn (two or more offered gateways).
    fn has_tabs(&self) -> bool {
        self.tab_count() > 1
    }

    /// The active offer (provider + content), when in tabbed mode. The
    /// sign-in dispatch reads the provider off this.
    pub fn active_offer(&self) -> Option<&'static GatewayOffer> {
        self.offers.get(self.active_tab)
    }

    /// Pre-select the tab of `provider`, when offered. Used by the
    /// dead-credential re-offer: the gateway that just died should be the
    /// tab the user lands on, not whichever happens to be first.
    /// Returns `true` when the tab was found and selected.
    pub fn select_provider_tab(&mut self, provider: &str) -> bool {
        if let Some(i) = self.offers.iter().position(|o| o.provider == provider) {
            self.active_tab = i;
            self.content = self.offers[i].content;
            true
        } else {
            false
        }
    }

    /// Cycle to the neighboring tab (question-dialog semantics): selection
    /// resets to Yes, the stream reveal carries over (the body text is
    /// shared across tabs by contract — see `GATEWAY_PITCH`), and any drag
    /// selection dies (its offsets address the old tab's document).
    fn switch_tab(&mut self, delta: isize) {
        let count = self.tab_count();
        if count < 2 {
            return;
        }
        let next = (self.active_tab as isize + delta).rem_euclid(count as isize) as usize;
        self.active_tab = next;
        self.content = self.offers[next].content;
        self.selected = 0;
        self.clear_selection();
    }

    /// Hide and reset the dialog.
    pub fn hide(&mut self) {
        self.visible = false;
        self.submitted = false;
        self.sign_in = None;
        self.selection = None;
        self.sel_width = 0;
    }

    /// Enter the `Connecting` phase in place: the box was just accepted
    /// (Yes) and STAYS OPEN — no hide/reopen flicker — until `Started`
    /// moves it to `Authorizing`.
    pub fn begin_connecting(&mut self) {
        self.visible = true;
        self.submitted = false;
        self.phase = Phase::Connecting;
        self.sign_in = None;
        self.selection = None;
        self.sel_width = 0;
    }

    /// Enter the `Authorizing` phase: the Yes/No options give way to the
    /// device sign-in display (code + URL, both drag-copyable). The box
    /// stays visible until [`Self::hide`] is called when the flow ends.
    pub fn begin_authorizing(&mut self, verification_url: &str, user_code: &str) {
        self.visible = true;
        self.submitted = false;
        self.phase = Phase::Authorizing;
        self.sign_in = Some((verification_url.to_string(), user_code.to_string()));
        self.selection = None;
        self.sel_width = 0;
    }

    /// The live device sign-in, if in the `Authorizing` phase.
    pub fn sign_in(&self) -> Option<(&str, &str)> {
        self.sign_in
            .as_ref()
            .map(|(url, code)| (url.as_str(), code.as_str()))
    }

    /// Whether the box is showing the device sign-in flow (`Connecting` or
    /// `Authorizing` phase). The transaction itself keeps polling in the
    /// background regardless of the box — Esc yields the screen (the code
    /// was already shown and is drag-copyable); this flag only marks the
    /// phases without Yes/No options to drive.
    pub fn is_authorizing(&self) -> bool {
        matches!(self.phase, Phase::Connecting | Phase::Authorizing)
    }

    /// Whether the streaming text is fully revealed.
    pub fn is_streaming_complete(&self) -> bool {
        self.phase == Phase::WaitingForResponse
    }

    /// Advance the streaming text by the elapsed time since the last call.
    /// Returns `true` if the offset changed (caller should redraw).
    pub fn advance_stream(&mut self) -> bool {
        if !self.visible || self.phase != Phase::Streaming {
            return false;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;

        let total = self.content.stream_text.chars().count();
        let chars_to_add = (elapsed * CHARS_PER_SECOND) as usize;
        let new_offset = (self.stream_chars + chars_to_add).min(total);

        if new_offset != self.stream_chars {
            self.stream_chars = new_offset;
            if self.stream_chars >= total {
                self.phase = Phase::WaitingForResponse;
            }
            true
        } else {
            false
        }
    }

    /// The revealed portion of the stream text (for rendering).
    /// Uses char indexing so the slice always lands on a valid UTF-8 boundary.
    fn revealed_text(&self) -> &str {
        // Find the byte offset for the stream_chars-th character.
        let byte_offset = self
            .content
            .stream_text
            .char_indices()
            .nth(self.stream_chars)
            .map_or(self.content.stream_text.len(), |(i, _)| i);
        &self.content.stream_text[..byte_offset]
    }

    // Height calculation

    /// EXACT height of the body: the count of `wrap_plain` rows the renderer
    /// actually draws (the markdown estimator is a different pipeline and can
    /// disagree by a row — here layout, render and hit-test share one wrap by
    /// construction). The body renders as PLAIN text in EVERY phase.
    fn body_height(&self, inner_w: u16) -> u16 {
        self.body_rows(inner_w).len().max(1) as u16
    }

    /// The wrapped plain-text body rows the renderer draws — the same wrap
    /// for layout, render and hit-test in every phase.
    fn body_rows(&self, inner_w: u16) -> Vec<String> {
        let plain =
            cosh_tui::core::renderables::markdown::markdown_to_visible_text(self.revealed_text());
        wrap_plain(&plain, inner_w)
    }

    /// Total required height for the dialog, including padding, text,
    /// question, options, and footer. 0 when hidden.
    pub fn required_height(&self, max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        let inner_w = max_width.saturating_sub(5); // pad(3) + 2

        // The body wraps as PLAIN text in every phase — measured with the
        // same wrap the renderer draws, so layout = render = hit-test.
        let text_h = self.body_height(inner_w);

        // The tab bar (question-dialog design) reserves a label row plus a
        // separator row between the top pad and the body. Single-content
        // mode reserves nothing.
        let tabs = u16::from(self.has_tabs()) * 2;

        match self.phase {
            Phase::Streaming => {
                // 1 top padding + text + cursor row + 1 bottom padding
                MIN_HEIGHT + tabs + text_h + 1
            }
            Phase::WaitingForResponse => {
                // 1 top padding + text + question(1) + 1 breathing gap
                // + options COLUMN (Yes 1 + No 1) + 1 footer gap
                // + footer(1) + 1 bottom padding
                MIN_HEIGHT + tabs + text_h + 1 + 1 + 1 + 2 + 1
            }
            Phase::Connecting => {
                // 1 top padding + body + hint row + 1 bottom padding
                MIN_HEIGHT + tabs + text_h + 2
            }
            Phase::Authorizing => {
                // 1 top padding + body + 1 gap + code line + url line
                // + 1 hint + 1 bottom padding
                MIN_HEIGHT + tabs + text_h + 4
            }
        }
    }

    // Keyboard input

    /// Handle a key event. Returns `true` if consumed.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }

        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // During streaming, skip navigation keys — only Esc and (tabbed
        // mode) the tab arrows are accepted: the tab bar is part of the
        // box's chrome in every phase, and the streamed body is shared
        // across tabs, so switching mid-stream is seamless (no rewind, no
        // restart) — that seamlessness is WHY the body is shared.
        if self.phase == Phase::Streaming {
            match code {
                KeyCode::Esc => return true,
                KeyCode::Left | KeyCode::Char('h') if self.has_tabs() => {
                    self.switch_tab(-1);
                    return true;
                }
                KeyCode::Right | KeyCode::Char('l') if self.has_tabs() => {
                    self.switch_tab(1);
                    return true;
                }
                _ => return false,
            }
        }

        // Connecting/Authorizing: no options to drive. Esc YIELDS the box to
        // the user — the device flow keeps polling in the background and
        // ends itself (Authorized/Failed hide + toast), so dismissing here
        // orphans nothing; it only gives the screen back. Ctrl+C is handled
        // by the App (copies the drag selection); anything else falls
        // through.
        if self.is_authorizing() {
            if code == KeyCode::Esc {
                self.visible = false;
                return true;
            }
            return ctrl;
        }

        // WaitingForResponse phase
        //
        // Tabbed mode splits the arrows' two jobs: Left/Right switch gateway
        // tabs (question-dialog semantics), Up/Down move the Yes/No
        // highlight — the options are a COLUMN, so the vertical arrows drive
        // them and the lateral arrows never fight the selection. Single-
        // content mode (no tabs) keeps Left/Right on Yes/No.
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                if self.has_tabs() {
                    self.switch_tab(-1);
                } else {
                    self.selected = 0;
                }
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.has_tabs() {
                    self.switch_tab(1);
                } else {
                    self.selected = 1;
                }
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = 0;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = 1;
                true
            }
            KeyCode::Char('1') => {
                self.selected = 0;
                self.submitted = true;
                true
            }
            KeyCode::Char('2') => {
                self.selected = 1;
                self.submitted = true;
                true
            }
            KeyCode::Enter => {
                self.submitted = true;
                true
            }
            KeyCode::Esc => {
                self.visible = false;
                // A dismissed dialog owns nothing: leave a stale selection
                // behind and a later Ctrl+C (gated on ownership, correctly)
                // would skip it forever — but nothing would ever clear it
                // either. Drop it here so dismissal is a full reset.
                self.clear_selection();
                true
            }
            KeyCode::Char(_) if ctrl => true,
            _ => false,
        }
    }

    // Selection (copy) ── same kernel as the question dialog: CHAR offsets
    // into each region's own text exactly as drawn, so hit-test, highlight
    // and copy can never drift apart.

    /// Whether a non-empty drag selection exists (Ctrl+C copies it).
    pub fn has_selection(&self) -> bool {
        self.selection.is_some_and(|s| s.is_active())
    }

    /// Anchor a selection at the copy-document offset `pos` (mouse press).
    /// `inner_w` freezes the wrap width so a mid-drag resize cannot point
    /// the offsets at other characters.
    pub fn begin_selection(&mut self, pos: usize, inner_w: u16) {
        self.selection = Some(DragSelection::anchor((), pos));
        self.sel_width = inner_w;
    }

    /// Extend the active selection's drag end to the document offset `pos`.
    /// No region checks — the document is ONE flat text, so a drag emends
    /// across rows (body → question, body → device code) instead of
    /// stopping at a region boundary.
    pub fn extend_selection(&mut self, pos: usize) {
        if let Some(sel) = self.selection.as_mut() {
            sel.extend(pos);
        }
    }

    /// Whether a mouse press anchored a selection on this dialog (active or
    /// still empty). The App's drag arm extends only then.
    pub fn has_selection_anchor(&self) -> bool {
        self.selection.is_some()
    }

    /// Drop the selection (copied, or the flow ended).
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The box's COPY DOCUMENT: every copyable row in visual order, as the
    /// renderer draws it. One flat text — body soft-wrapped rows first,
    /// then the hard rows (offer title, or device code + URL) — so a single
    /// char range can span body → question or body → code and copy is one
    /// contiguous slice. Layout, hit-test, highlight and copy all read
    /// THIS, so they can never drift apart. `inner_w` is the wrap width the
    /// caller's rect provides (frozen at anchor time via `sel_width`).
    fn copy_doc(&self, inner_w: u16) -> Vec<(String, CopyRowKind)> {
        let mut doc: Vec<(String, CopyRowKind)> = self
            .body_rows(inner_w)
            .into_iter()
            .map(|r| (r, CopyRowKind::Body))
            .collect();
        match self.phase {
            Phase::Authorizing => {
                if let Some((url, code)) = self.sign_in.as_ref() {
                    doc.push((code.clone(), CopyRowKind::Hard));
                    doc.push((url.clone(), CopyRowKind::Hard));
                }
            }
            // Offer phase only — Streaming/Connecting have no title row.
            Phase::WaitingForResponse => {
                doc.push((self.content.title.to_string(), CopyRowKind::Hard));
            }
            Phase::Streaming | Phase::Connecting => {}
        }
        doc
    }

    /// The selected text, sliced from the copy document: soft-wrapped body
    /// rows re-join with single spaces (soft wraps are not real lines),
    /// hard rows (title/code/URL) join with newlines. Empty when nothing is
    /// selected.
    pub fn selected_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        let doc = self.copy_doc(self.sel_width);
        let total: usize = doc.iter().map(|(r, _)| r.chars().count()).sum();
        let Some((s, e)) = sel.range_for((), total) else {
            return String::new();
        };
        let mut out = String::new();
        let mut consumed = 0usize;
        for (row, kind) in &doc {
            let len = row.chars().count();
            let row_end = consumed + len;
            if len > 0 && row_end > s && consumed < e {
                let a = s.saturating_sub(consumed).min(len);
                let b = e.saturating_sub(consumed).min(len);
                if b > a && !out.is_empty() {
                    // Hard rows are real lines; soft-wrapped body rows are
                    // one paragraph and re-join with spaces.
                    out.push(if *kind == CopyRowKind::Hard {
                        '\n'
                    } else {
                        ' '
                    });
                }
                if b > a {
                    out.push_str(&row.chars().skip(a).take(b - a).collect::<String>());
                }
            }
            consumed = row_end;
        }
        out
    }

    /// Resolve the copy-document offset a mouse press/drag targets, given
    /// the SAME `area` the render call received. Rows CLAMP to the nearest
    /// copyable row instead of rejecting — the App consumes any press in
    /// the drawn box, and a `None` here would fall through to the chat's
    /// drag selection (the original leak). A gap/pad/options row anchors
    /// the BOUNDARY between its neighbors — neighboring rows' document
    /// offsets coincide there — so drags emend across rows: body →
    /// question, body → device code, one contiguous selection.
    pub fn selection_target_at(&self, x: u16, y: u16, area: Rect) -> Option<usize> {
        if !self.visible {
            return None;
        }
        let inner_x = area.x + 3;
        let inner_w = area.width.saturating_sub(5);
        // While an anchor is live, offsets must address the FROZEN wrap
        // (`sel_width`), not the current area: a mid-drag resize would
        // otherwise point new drag offsets at characters in a different
        // wrap than the one the copy slices.
        let doc_w = if self.selection.is_some() {
            self.sel_width
        } else {
            inner_w
        };
        let doc = self.copy_doc(doc_w);
        if doc.is_empty() {
            return None;
        }
        // Screen y of each document row — the SAME layout the renderer
        // draws: the tab bar (when drawn) takes the first label row plus a
        // separator, body rows follow from the top pad, ONE gap row, then
        // the hard rows (device code + URL in Authorizing, the question
        // title in the offer phase).
        let text_h = self.body_height(inner_w);
        let tabs = u16::from(self.has_tabs()) * 2;
        let body_start = area.y + 1 + tabs;
        let hard_y = body_start + text_h + 1;
        let title_y = {
            let height = self.required_height(area.width).min(area.height);
            // Mirror render's offer branch: footer at height-2, the
            // question FIVE rows above it (breathing gap + the Yes/No
            // COLUMN: Yes at footer-3, No at footer-2 + footer gap).
            (area.y + height.saturating_sub(2)).saturating_sub(5)
        };
        let mut rows: Vec<(u16, usize, usize, u16)> = Vec::with_capacity(doc.len()); // (y, doc_start, len, x_prefix)
        let mut off = 0usize;
        for (i, (row, kind)) in doc.iter().enumerate() {
            let len = row.chars().count();
            // Body rows draw at the inner edge; the code/URL rows draw with
            // a 2-cell prefix (the doc text is the bare value).
            let (sy, x_prefix) = match kind {
                CopyRowKind::Body => (body_start + i as u16, 0u16),
                CopyRowKind::Hard if self.phase == Phase::Authorizing => {
                    // Invariant: copy_doc emits exactly `text_h` body rows
                    // (wrap_plain never returns zero), so `i` is never
                    // below `text_h` here — saturating pins it anyway.
                    (hard_y + i.saturating_sub(text_h as usize) as u16, 2u16)
                }
                CopyRowKind::Hard => (title_y, 0u16),
            };
            rows.push((sy, off, len, x_prefix));
            off += len;
        }
        // Columns clamp into the inner band; the ROW is the last one at or
        // above the pointer (rows are in visual order), clamped to the
        // drawable rect — a press below the last copyable row anchors its
        // end, a press on the gap anchors the boundary.
        if x < area.x || x >= area.x + area.width {
            return None;
        }
        let col = (x.saturating_sub(inner_x) as usize).min(inner_w as usize);
        let box_bottom = area.y + area.height.saturating_sub(1);
        let y = y.clamp(body_start, box_bottom.max(body_start));
        let (_, row_start, row_len, x_prefix) = rows
            .iter()
            .rfind(|(sy, ..)| *sy <= y)
            .or_else(|| rows.first())?;
        // Column beyond the row's own text (past its end, or inside its
        // 2-cell prefix) anchors the boundary — the nearest doc char.
        Some(row_start + col.saturating_sub(*x_prefix as usize).min(*row_len))
    }

    /// Handle a mouse click. Returns `true` if consumed.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect, _theme: &Theme) -> bool {
        if !self.visible {
            return false;
        }

        let x = mouse.x;
        let y_click = mouse.y;
        let height = self.required_height(area.width).min(area.height);

        // Outside dialog bounds → not consumed (caller dismisses)
        if x < area.x || x >= area.x + area.width || y_click < area.y || y_click >= area.y + height
        {
            return false;
        }

        // The Connecting/Authorizing box has no clickable actions: presses
        // inside it anchor copy selections (App-side, via
        // `selection_target_at`) and are swallowed here so they never reach
        // the content underneath. Presses OUTSIDE still fall through
        // (dismissal semantics of the caller stay intact for the offer
        // phases).
        if self.is_authorizing() {
            return true;
        }

        let inner_x = area.x + 3;
        let _inner_w = area.width.saturating_sub(5);

        // Click on the tab bar (question-dialog design): " Label " chips on
        // the first inner row. Streaming and the offer phase — the tab bar
        // is chrome in every NAVIGABLE phase (keyboard Left/Right switch
        // tabs mid-stream too; mouse must not lag the keyboard).
        // Connecting/Authorizing pin the provider whose sign-in is running
        // (handled above) and never reach this arm.
        if self.has_tabs() && y_click == area.y + 1 {
            let mut tx = inner_x;
            for (i, offer) in self.offers.iter().enumerate() {
                let label_len = format!(" {} ", offer.content.tab_label).chars().count() as u16;
                if x >= tx && x < tx + label_len {
                    if i != self.active_tab {
                        // Left/Right parity with the keyboard path: reset to
                        // Yes, drop the selection (old tab's offsets).
                        self.switch_tab(if i > self.active_tab { 1 } else { -1 });
                    }
                    return true;
                }
                tx += label_len + 1;
            }
            return true;
        }

        // Streaming: presses inside the box anchor a copy selection (the
        // body is drag-copyable from the first revealed character) — a
        // click no longer dismisses; Esc still does. Clicks outside keep
        // the caller's dismissal semantics via the fall-through.
        if self.phase == Phase::Streaming {
            return true;
        }

        // WaitingForResponse: hit-test the Yes/No options and the footer
        let footer_y = area.y + height.saturating_sub(2);

        // The question and options are at fixed positions relative to
        // footer — mirroring the render branch: question at footer-5, then
        // the Yes/No COLUMN (Yes at footer-3, No at footer-2).
        let yes_y = footer_y.saturating_sub(3);
        let no_y = footer_y.saturating_sub(2);

        // Click on the Yes/No column (each option's row, indicator + label)
        if y_click == yes_y && x >= inner_x && x < inner_x + 5 {
            self.selected = 0;
            self.submitted = true;
            return true;
        }
        if y_click == no_y && x >= inner_x && x < inner_x + 4 {
            self.selected = 1;
            self.submitted = true;
            return true;
        }

        // Click on footer: "enter" or "esc"
        if y_click == footer_y {
            if x >= inner_x && x < inner_x + 5 {
                // "enter"
                self.submitted = true;
                return true;
            }
            // Render draws " confirm" spanning inner_x+5..inner_x+13 and
            // " esc" from inner_x+14 — the hit-test must read the SAME
            // columns (an overlap here dismissed on clicks meant for the
            // label text).
            let esc_x = inner_x + 14;
            if x >= esc_x && x < esc_x + 3 {
                // "esc"
                self.visible = false;
                return true;
            }
        }

        true
    }

    // Rendering

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let height = self.required_height(area.width).min(area.height);
        let inner_area = Rect::new(area.x, area.y, area.width, height);

        wall::render(buf, inner_area, theme.accent, theme.background_panel);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let footer_y = area.y + height.saturating_sub(2);

        // Tab bar (question-dialog design): one label row plus a separator
        // row between the top pad and the body. Tabbed mode only.
        let tab_y = area.y + 1;
        let tabs = u16::from(self.has_tabs()) * 2;

        // Body: PLAIN text (markdown stripped) wrapped at inner_w — the
        // exact rows `selection_target_at` addresses, in EVERY phase
        // (streaming included): drag-copy works from the first revealed
        // character. Below the tab bar when tabs are drawn.
        let text_h = self.body_height(inner_w);
        let text_y = area.y + 1 + tabs;

        let rows = self.body_rows(inner_w);
        let base_style = Style::default().fg(rgba_color(theme.text));

        // Tab bar labels (question-dialog design): " Label " chips on the
        // first inner row, the active one bold in the accent color, the
        // rest muted; a blank separator row divides bar and body. Drawn in
        // EVERY phase (the bar is part of the box's chrome, and the shared
        // stream body makes a mid-stream switch seamless). Hit-tested in
        // the offer phase only — Connecting/Authorizing pin the provider
        // whose sign-in is running.
        if self.has_tabs() {
            let mut tx = inner_x;
            for (i, offer) in self.offers.iter().enumerate() {
                let label = format!(" {} ", offer.content.tab_label);
                let style = if i == self.active_tab {
                    Style::default()
                        .fg(rgba_color(theme.accent))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(rgba_color(theme.text_muted))
                };
                draw_text_line(
                    buf,
                    &label,
                    tx,
                    tab_y,
                    inner_w.saturating_sub(tx - inner_x),
                    style,
                );
                tx += label.chars().count() as u16 + 1;
            }
            // Separator row: intentionally blank — the wall background
            // already separates the bar from the body; the ROW is what the
            // required-height budget reserves (tabs = 2).
        }

        let sel_style = Style::default()
            .fg(rgba_color(theme.background_panel))
            .bg(rgba_color(theme.text))
            .add_modifier(Modifier::BOLD);
        // ONE selection range over the copy document (body rows + the hard
        // rows below): every highlight in every phase slices this span.
        let doc = self.copy_doc(inner_w);
        let doc_total: usize = doc.iter().map(|(r, _)| r.chars().count()).sum();
        let sel_span = self
            .selection
            .as_ref()
            .and_then(|sel| sel.range_for((), doc_total));
        let body_total: usize = rows.iter().map(|r| r.chars().count()).sum();
        for (ri, row) in rows.iter().enumerate() {
            let y = text_y + ri as u16;
            draw_text_line(buf, row, inner_x, y, inner_w, base_style);
            // Selection highlight over this row's span, if the drag
            // covers it (same range kernel the copy path reads).
            if let Some((s, e)) = sel_span {
                let total_before: usize = rows.iter().take(ri).map(|r| r.chars().count()).sum();
                if e > total_before && s < total_before + row.chars().count() {
                    let a = s.saturating_sub(total_before).min(row.chars().count()) as u16;
                    // Clamp to the DRAWN width: wrap_plain never hard-splits
                    // an over-long word, so a row can exceed inner_w — the
                    // tail is clipped by draw_text_line and must not be
                    // painted over the border/padding.
                    let b = e
                        .saturating_sub(total_before)
                        .min(row.chars().count())
                        .min(inner_w as usize) as u16;
                    for cx in (inner_x + a)..(inner_x + b) {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_style(sel_style);
                        }
                    }
                }
            }
        }

        if self.phase == Phase::Streaming {
            // Blinking cursor below the text while streaming
            let cursor_y = text_y + text_h;
            if cursor_y < footer_y {
                let now = std::time::SystemTime::now();
                let blink = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    % 1000
                    < 500; // toggle every 500ms
                if blink {
                    let style = Style::default().fg(rgba_color(theme.text_muted));
                    draw_text_line(buf, "\u{2588}", inner_x, cursor_y, 1, style);
                }
            }
        } else if self.phase == Phase::Connecting {
            // Opt-in accepted, device flow starting: keep the box EXACTLY
            // where it is (same body, same geometry) — only the options
            // rows give way to a "connecting" hint. No hide/reopen flicker.
            let hint_y = text_y + text_h + 1;
            let box_bottom = area.y + height.saturating_sub(1);
            if hint_y <= box_bottom {
                // Provider-neutral: the ACTIVE tab's gateway is the one
                // whose device flow is starting (GitHub tab opted in →
                // "connecting to the GitHub Copilot gateway…").
                let gateway = self
                    .active_offer()
                    .map_or("gateway", |offer| offer.content.tab_label);
                draw_text_line(
                    buf,
                    &format!("connecting to the {gateway} gateway…"),
                    inner_x,
                    hint_y,
                    inner_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );
            }
        } else if self.phase == Phase::Authorizing {
            // Device sign-in display: the body (drawn above, selectable in
            // every phase) stays, and the options rows give way to the
            // device code + verification URL — the two things the user must
            // read and paste. All three are drag-copyable.
            let Some((url, code)) = self.sign_in.as_ref() else {
                return;
            };
            let code_y = text_y + text_h + 1;
            let url_y = code_y + 1;
            let hint_y = url_y + 1;
            // The rect is height-clamped (`height.min(area.height)`): on a
            // short terminal the tail rows fall below the box — never paint
            // them over the content underneath.
            let box_bottom = area.y + height.saturating_sub(1);

            // Device code: prominent, bold, own line — the ONE thing the
            // user must get right.
            let code_line = format!("  {code}");
            if code_y <= box_bottom {
                draw_text_line(
                    buf,
                    &code_line,
                    inner_x,
                    code_y,
                    inner_w,
                    Style::default()
                        .fg(rgba_color(theme.primary))
                        .add_modifier(Modifier::BOLD),
                );
                // Highlight slice of the code row within the ONE document
                // span (code is the first hard row: doc offset body_total;
                // the row is DRAWN with a 2-cell prefix, the doc text is not).
                if let Some((s, e)) = sel_span {
                    let a = s.saturating_sub(body_total).min(code.chars().count()) as u16;
                    // Clamp to the drawn width (inner_w minus the 2-cell
                    // prefix): an over-long value clips at draw time and
                    // must not paint over the border/padding.
                    let b = e
                        .saturating_sub(body_total)
                        .min(code.chars().count())
                        .min(inner_w.saturating_sub(2) as usize) as u16;
                    let sel_style = Style::default()
                        .fg(rgba_color(theme.background_panel))
                        .bg(rgba_color(theme.primary));
                    for cx in (inner_x + 2 + a)..(inner_x + 2 + b) {
                        if let Some(cell) = buf.cell_mut((cx, code_y)) {
                            cell.set_style(sel_style);
                        }
                    }
                }
            }

            // Verification URL.
            let url_line = format!("  {url}");
            if url_y <= box_bottom {
                draw_text_line(
                    buf,
                    &url_line,
                    inner_x,
                    url_y,
                    inner_w,
                    Style::default().fg(rgba_color(theme.info)),
                );
                // Highlight slice of the URL row within the ONE document
                // span (URL is the SECOND hard row: body_total + code len).
                if let Some((s, e)) = sel_span {
                    let url_start = body_total + code.chars().count();
                    let a = s.saturating_sub(url_start).min(url.chars().count()) as u16;
                    // Clamp to the drawn width (inner_w minus the 2-cell
                    // prefix): an over-long value clips at draw time and
                    // must not paint over the border/padding.
                    let b = e
                        .saturating_sub(url_start)
                        .min(url.chars().count())
                        .min(inner_w.saturating_sub(2) as usize) as u16;
                    let sel_style = Style::default()
                        .fg(rgba_color(theme.background_panel))
                        .bg(rgba_color(theme.info));
                    for cx in (inner_x + 2 + a)..(inner_x + 2 + b) {
                        if let Some(cell) = buf.cell_mut((cx, url_y)) {
                            cell.set_style(sel_style);
                        }
                    }
                }
            }

            // Copy hint — drag-and-drop semantics, same as the question
            // dialog's selection: drag over the text, release copies it.
            if hint_y <= box_bottom {
                draw_text_line(
                    buf,
                    "drag over the code to select — release to copy",
                    inner_x,
                    hint_y,
                    inner_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );
            }
        } else {
            // Question + Yes/No COLUMN — one breathing gap row between the
            // question and the options, and the options stacked vertically
            // (Yes above No) so the vertical arrows drive them and the
            // lateral arrows stay free for tab switching. (footer-1 holds
            // the gap between No and the footer.)
            let question_y = footer_y.saturating_sub(5);
            let yes_y = footer_y.saturating_sub(3);
            let no_y = footer_y.saturating_sub(2);

            // Question text — drag-copyable (the last hard row of the
            // copy document in the offer phase).
            let q_style = Style::default().fg(rgba_color(theme.text));
            draw_text_line(
                buf,
                self.content.title,
                inner_x,
                question_y,
                inner_w,
                q_style,
            );
            // Highlight slice of the title row within the ONE document
            // span (the title is the only hard row in the offer phase:
            // doc offset body_total; no 2-cell prefix on this row).
            if let Some((s, e)) = sel_span {
                let a = s
                    .saturating_sub(body_total)
                    .min(self.content.title.chars().count()) as u16;
                // Clamp to the drawn width: an over-long title clips at
                // draw time and must not paint over the border/padding.
                let b = e
                    .saturating_sub(body_total)
                    .min(self.content.title.chars().count())
                    .min(inner_w as usize) as u16;
                let sel_style = Style::default()
                    .fg(rgba_color(theme.background_panel))
                    .bg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                for cx in (inner_x + a)..(inner_x + b) {
                    if let Some(cell) = buf.cell_mut((cx, question_y)) {
                        cell.set_style(sel_style);
                    }
                }
            }

            // Options: a Yes/No COLUMN (Yes above No), both at the inner
            // edge — the question-dialog option column, so Up/Down drives
            // the highlight and Left/Right switch tabs in tabbed mode.
            let yes_selected = self.selected == 0;
            let no_selected = self.selected == 1;

            // Yes
            let yes_indicator = if yes_selected { "\u{1F7B4} " } else { "  " };
            let yes_style = if yes_selected {
                Style::default().fg(rgba_color(theme.success))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, yes_indicator, inner_x, yes_y, 2, yes_style);
            let yes_label_style = if yes_selected {
                Style::default().fg(rgba_color(theme.success))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, "Yes", inner_x + 2, yes_y, 3, yes_label_style);

            // No — own row, directly below Yes
            let no_indicator = if no_selected { "\u{1F7B4} " } else { "  " };
            let no_style = if no_selected {
                Style::default().fg(rgba_color(theme.error))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, no_indicator, inner_x, no_y, 2, no_style);
            let no_label_style = if no_selected {
                Style::default().fg(rgba_color(theme.error))
            } else {
                Style::default().fg(rgba_color(theme.text_muted))
            };
            draw_text_line(buf, "No", inner_x + 2, no_y, 2, no_label_style);

            // Footer
            let bg = rgba_color(theme.background_panel);
            let key_fg = rgba_color(theme.text);
            let desc_fg = rgba_color(theme.text_muted);

            draw_text_line(
                buf,
                "enter",
                inner_x,
                footer_y,
                inner_w,
                Style::default().fg(key_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " confirm",
                inner_x + 5,
                footer_y,
                inner_w.saturating_sub(5),
                Style::default().fg(desc_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " esc",
                inner_x + 14,
                footer_y,
                inner_w.saturating_sub(14),
                Style::default().fg(key_fg).bg(bg),
            );
            draw_text_line(
                buf,
                " dismiss",
                inner_x + 18,
                footer_y,
                inner_w.saturating_sub(18),
                Style::default().fg(desc_fg).bg(bg),
            );
        }
    }
}

// Helpers

/// Word-wrap plain text to `width` columns, preserving paragraph breaks.
/// Same algorithm as the question dialog's `wrap_text`: soft-wrapped rows
/// re-join with single spaces on copy, blank paragraphs become blank rows.
fn wrap_plain(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line: String = String::new();

    for (pi, paragraph) in text.split('\n').enumerate() {
        if pi > 0 && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }

        for word in paragraph.split_whitespace() {
            let word: String = word.chars().filter(|c| !c.is_control()).collect();
            if word.is_empty() {
                continue;
            }
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        // The paragraph's remainder (possibly empty) ends its row.
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        } else if pi > 0 || text.split('\n').count() > 1 {
            lines.push(String::new());
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        crate::theme::ThemeRegistry::new().default_theme().clone()
    }

    /// Sample content for exercising the generic dialog (the shipped
    /// `PLACEHOLDER` is intentionally empty). Not part of [`OFFERS`] —
    /// the dialog must stay generic over ANY content.
    const SAMPLE: GatewayRecommendationContent = GatewayRecommendationContent {
        tab_label: "Sample",
        title: "Use the sample free gateway?",
        stream_text: "Sample recommendation body used to exercise the generic dialog. \
            It spans several sentences so streaming advances over multiple frames.",
    };

    #[test]
    fn hidden_dialog_has_zero_height() {
        let d = FreeGatewayRecommendationDialog::new();
        assert_eq!(d.required_height(80), 0);
    }

    #[test]
    fn streaming_height_includes_cursor() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        assert!(d.visible);
        let h = d.required_height(80);
        assert!(h >= MIN_HEIGHT + 2); // text(1+) + cursor(1) + padding(2)
    }

    #[test]
    fn response_height_includes_options_and_footer() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        // Force streaming complete
        d.phase = Phase::WaitingForResponse;
        d.stream_chars = d.content.stream_text.chars().count();
        let h = d.required_height(80);
        // Two body rows + question + question gap + options + footer gap
        // + footer + top/bottom padding.
        assert_eq!(h, 10);
    }

    #[test]
    fn advance_stream_progresses_offset() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        assert_eq!(d.stream_chars, 0);
        // Simulate some time passing
        d.last_frame = Instant::now() - std::time::Duration::from_millis(500);
        let changed = d.advance_stream();
        assert!(changed);
        assert!(d.stream_chars > 0);
    }

    #[test]
    fn advance_stream_completes_after_enough_time() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        let total = d.content.stream_text.chars().count();
        // Simulate enough time for all characters
        let secs = total as f64 / CHARS_PER_SECOND + 0.1;
        d.last_frame = Instant::now() - std::time::Duration::from_secs_f64(secs);
        d.advance_stream();
        assert_eq!(d.phase, Phase::WaitingForResponse);
        assert_eq!(d.stream_chars, total);
    }

    #[test]
    fn enter_submits_and_esc_dismisses() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.phase = Phase::WaitingForResponse;
        d.stream_chars = d.content.stream_text.chars().count();

        assert!(!d.submitted);
        d.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(d.submitted);

        let mut d2 = FreeGatewayRecommendationDialog::new();
        d2.show(&SAMPLE);
        d2.phase = Phase::WaitingForResponse;
        d2.stream_chars = d2.content.stream_text.chars().count();
        d2.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!d2.visible);
    }

    #[test]
    fn hidden_dialog_consumes_nothing() {
        let mut d = FreeGatewayRecommendationDialog::new();
        assert!(!d.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn streaming_body_is_drag_copyable() {
        // The user asked for copy DURING streaming: anchor mid-body via the
        // hit-test the render shares, extend, then copy — the revealed text
        // must come back (no sign_in dependency, no phase gate).
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.last_frame = Instant::now() - std::time::Duration::from_millis(500);
        d.advance_stream();
        assert_eq!(d.phase, Phase::Streaming);
        assert!(d.stream_chars > 0);

        let area = Rect::new(0, 0, 80, 24);
        let Some(anchor) = d.selection_target_at(10, 1, area) else {
            panic!("the body band must be hit-testable while streaming");
        };
        d.begin_selection(anchor, area.width.saturating_sub(5));
        let Some(end) = d.selection_target_at(20, 1, area) else {
            panic!("the drag end must be hit-testable too");
        };
        d.extend_selection(end);

        let copied = d.selected_text();
        assert!(
            !copied.is_empty(),
            "a streaming-phase body selection must copy the revealed text"
        );
        assert!(
            d.revealed_text().contains(&copied),
            "the copied slice must come from the revealed stream text: {copied:?}"
        );
    }

    #[test]
    fn esc_yields_box_in_connecting_and_authorizing() {
        // Anti-flicker contract: after opt-in the box stays up (Connecting,
        // options give way to a hint), body copy still works — and Esc now
        // YIELDS the box in every phase (the background flow ends itself).
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.stream_chars = d.content.stream_text.chars().count();
        d.begin_connecting();
        assert!(d.visible);
        assert_eq!(d.phase, Phase::Connecting);
        assert!(d.is_authorizing());

        assert!(d.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!d.visible, "Esc must yield the box during Connecting too");

        // Started then arrives: the flow is still running, but the user
        // asked the box away — the pump must NOT force it back open.
        assert!(!d.visible, "no ghost reopen after the user dismissed");

        // A fresh box (no Esc) reaches Authorizing with the code displayed.
        let mut d2 = FreeGatewayRecommendationDialog::new();
        d2.show(&SAMPLE);
        d2.stream_chars = d2.content.stream_text.chars().count();
        d2.begin_connecting();
        d2.begin_authorizing("https://hyper.charm.land/device", "ABCD-1234");
        assert_eq!(d2.phase, Phase::Authorizing);
        assert!(d2.visible);
        assert_eq!(
            d2.sign_in(),
            Some(("https://hyper.charm.land/device", "ABCD-1234"))
        );

        // And Esc yields the Authorizing box as well (the reported bug).
        assert!(d2.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!d2.visible, "Esc must yield the box during Authorizing");
    }

    #[test]
    fn drag_from_code_upwards_emends_into_the_body() {
        // Bug report: dragging UP from the last word of the code line
        // (crossing into the body) copied nothing. In the copy-document
        // model the drag EMENDS: anchor on the code's tail, extend up into
        // the body — the copy is one contiguous slice ending at the code's
        // end (no region clamp, no discarded movement).
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.stream_chars = d.content.stream_text.chars().count();
        d.begin_authorizing("https://hyper.charm.land/device", "ABCD-1234");
        assert_eq!(d.phase, Phase::Authorizing);

        let area = Rect::new(0, 0, 80, 24);
        let inner_w = area.width.saturating_sub(5);
        let text_h = d.body_height(inner_w);
        let code_y = 1 + text_h + 1; // body_start(1) + text_h + gap(1)
        let code_len = "ABCD-1234".chars().count();
        // Anchor at the code's END boundary — one cell past the last char
        // (row drawn with a 2-cell prefix; the hit-test clamps past-end
        // presses to the row's length, so the slice INCLUDES the final
        // '4' — char offsets select up to the anchor, not through it)...
        let Some(anchor) = d.selection_target_at(3 + 2 + code_len as u16, code_y, area) else {
            panic!("the code row must be hit-testable while authorizing");
        };
        d.begin_selection(anchor, inner_w);
        // ...and drag UP into the body band — one contiguous selection.
        let Some(end) = d.selection_target_at(10, 1, area) else {
            panic!("the body band must stay hit-testable during the drag");
        };
        assert!(end < anchor, "the upward drag must address body offsets");
        d.extend_selection(end);

        // The copy ends at the code's end: last two chars are "34".
        let copied = d.selected_text();
        assert!(
            copied.ends_with("34"),
            "the emended selection must end at the code's tail, got {copied:?}"
        );
        assert!(
            copied.contains('\n'),
            "crossing from the body into the code line joins with a newline: {copied:?}"
        );
    }

    #[test]
    fn press_one_cell_before_the_first_char_still_anchors() {
        // Bug report: copying "No API key is configured" required hitting
        // the 'N' exactly — a press one cell before it (on the pad) was
        // rejected and fell through. The hit-test must clamp the column
        // into the inner band.
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.last_frame = Instant::now() - std::time::Duration::from_millis(500);
        d.advance_stream();
        assert!(d.stream_chars > 0);

        let area = Rect::new(0, 0, 80, 24);
        // x = 2 is ONE cell left of inner_x (3): previously None.
        let Some(anchor) = d.selection_target_at(2, 1, area) else {
            panic!("a press on the pad before the first char must anchor");
        };
        assert_eq!(anchor, 0, "the clamped column must address the first char");
        d.begin_selection(anchor, area.width.saturating_sub(5));
        let Some(end) = d.selection_target_at(12, 1, area) else {
            panic!("the drag end must be hit-testable too");
        };
        d.extend_selection(end);
        let copied = d.selected_text();
        assert!(
            !copied.is_empty(),
            "a pad-anchored selection must still copy text"
        );
        assert!(
            d.revealed_text().contains(&copied),
            "the copied slice must come from the revealed stream text: {copied:?}"
        );
    }

    #[test]
    fn offer_title_is_drag_copyable() {
        // Last gap: the offer-phase question ("Sign in to the Charm gateway
        // with OAuth?") had no copy region — a drag over it selected the
        // chat instead. It must anchor the Title region and copy verbatim.
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.stream_chars = d.content.stream_text.chars().count();
        // In-module: pin the offer phase directly (the stream tick only
        // transitions when it reveals NEW chars, which is orthogonal here).
        d.phase = Phase::WaitingForResponse;
        assert_eq!(d.phase, Phase::WaitingForResponse);

        let area = Rect::new(0, 0, 80, 24);
        let height = d.required_height(area.width).min(area.height);
        let question_y = area.y + height.saturating_sub(2) - 5; // footer - 5 (question row)
        let Some(anchor) = d.selection_target_at(5, question_y, area) else {
            panic!("the title row must be hit-testable in the offer phase");
        };
        d.begin_selection(anchor, area.width.saturating_sub(5));
        // Drag through the last word (beyond the title's end — clamps).
        let Some(end) = d.selection_target_at(60, question_y, area) else {
            panic!("the title drag end must be hit-testable too");
        };
        d.extend_selection(end);

        // `anchor`/`end` are copy-DOCUMENT offsets; the title is the last
        // hard row, so its own slice is anchor..end minus the body length.
        let body_total: usize = d
            .body_rows(area.width.saturating_sub(5))
            .iter()
            .map(|r| r.chars().count())
            .sum();
        let (t0, t1) = (anchor - body_total, end - body_total);
        let copied = d.selected_text();
        assert_eq!(
            copied,
            SAMPLE
                .title
                .chars()
                .skip(t0)
                .take(t1 - t0)
                .collect::<String>(),
            "the copied slice must be the title's anchor..end"
        );
    }

    #[test]
    fn drag_from_body_into_the_question_emends_one_copy() {
        // THE refactor's reason (#2179): "não consigo emendar no copy de
        // cima; ele para de copiar quando chega na pergunta". The document
        // is ONE flat text — a drag from mid-body down through the gap into
        // the question row must produce a single contiguous copy: body tail
        // (soft-wrapped rows re-joined with spaces) + newline + title slice.
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.stream_chars = d.content.stream_text.chars().count();
        d.phase = Phase::WaitingForResponse; // offer phase, in-module

        let area = Rect::new(0, 0, 80, 24);
        let inner_w = area.width.saturating_sub(5);
        let height = d.required_height(area.width).min(area.height);
        let question_y = area.y + height.saturating_sub(2) - 5; // footer - 5 (question row)
        // Anchor mid-body (row 0, past the first chars)...
        let Some(anchor) = d.selection_target_at(10, 1, area) else {
            panic!("the body band must be hit-testable in the offer phase");
        };
        d.begin_selection(anchor, inner_w);
        // ...and drag DOWN into the question row, past its end (clamps).
        let Some(end) = d.selection_target_at(60, question_y, area) else {
            panic!("the question row must be hit-testable during the drag");
        };
        d.extend_selection(end);

        let copied = d.selected_text();
        assert!(!copied.is_empty(), "the emended copy must not be empty");
        // One newline exactly at the body→title boundary, title at the end.
        let title = SAMPLE.title;
        assert!(
            copied.ends_with(title),
            "the emended copy must end with the full title, got {copied:?}"
        );
        let (body_part, _) = copied
            .rsplit_once('\n')
            .expect("crossing body→question must join with ONE newline");
        assert!(
            !body_part.contains('\n'),
            "soft-wrapped body rows re-join with spaces, not newlines: {copied:?}"
        );
        assert!(
            d.revealed_text()
                .contains(body_part.split(' ').next_back().unwrap_or_default()),
            "the body tail of the copy must come from the revealed text: {copied:?}"
        );
    }

    /// REGRESSION (stale-selection leak): Esc dismisses the box but used to
    /// keep the drag selection alive — a hidden selection that nothing
    /// would ever clear. Dismissal must be a full reset.
    #[test]
    fn esc_dismissal_clears_the_drag_selection() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.phase = Phase::WaitingForResponse;
        d.stream_chars = d.content.stream_text.chars().count();

        let area = Rect::new(0, 0, 80, 24);
        let Some(anchor) = d.selection_target_at(10, 1, area) else {
            panic!("the body band must be hit-testable");
        };
        d.begin_selection(anchor, area.width.saturating_sub(5));
        // An anchor alone is not an ACTIVE selection yet — extend it, same
        // as a real drag's first move.
        let Some(end) = d.selection_target_at(20, 1, area) else {
            panic!("the drag end must be hit-testable too");
        };
        d.extend_selection(end);
        assert!(d.has_selection(), "fixture needs a live selection");

        d.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!d.visible);
        assert!(
            !d.has_selection(),
            "a dismissed dialog must not carry a stale selection"
        );
        assert!(
            !d.has_selection_anchor(),
            "the anchor must die with the dismissal too"
        );
    }

    /// REGRESSION (tabbed offer): with two or more offered gateways the
    /// arrows SPLIT jobs — Left/Right switch tabs (question-dialog
    /// semantics), Up/Down drive the Yes/No column. The stream reveal is
    /// shared across tabs, so a switch never rewinds it, and the Yes/No
    /// selection resets to Yes.
    #[test]
    fn tabbed_offer_switches_tabs_without_touching_the_stream() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show_offers(OFFERS);
        assert!(d.has_tabs(), "fixture: OFFERS must render a tab bar");
        assert_eq!(
            d.active_offer().unwrap().provider,
            "charm",
            "tab order = OFFERS order"
        );
        d.stream_chars = 40; // mid-stream reveal
        d.phase = Phase::WaitingForResponse;

        // Right → next tab; the reveal carries over, the selection resets.
        d.selected = 1;
        assert!(d.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        assert_eq!(d.active_offer().unwrap().provider, "copilot");
        assert_eq!(d.active_offer().unwrap().content.title, GITHUB.title);
        assert_eq!(
            d.stream_chars, 40,
            "the shared body must not rewind on a switch"
        );
        assert_eq!(
            d.selected, 0,
            "a tab switch resets the Yes/No selection to Yes"
        );

        // Left wraps back around.
        assert!(d.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert_eq!(d.active_offer().unwrap().provider, "charm");

        // Up/Down drive the COLUMN; Left/Right no longer touch it.
        d.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(d.selected, 1, "Down moves the highlight to No");
        d.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(d.selected, 0, "Up moves the highlight back to Yes");
        d.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            d.active_offer().unwrap().provider,
            "copilot",
            "Right switches tabs, never the Yes/No selection"
        );
        assert_eq!(d.selected, 0);
    }

    /// A tab switch drops any live drag selection: its char offsets address
    /// the OLD tab's copy document, and carrying them over would silently
    /// copy the new tab's text.
    #[test]
    fn tab_switch_drops_the_drag_selection() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show_offers(OFFERS);
        d.stream_chars = d.content.stream_text.chars().count();
        d.phase = Phase::WaitingForResponse;

        let area = Rect::new(0, 0, 80, 24);
        // Tabbed offer: body starts after the tab bar (label row + separator).
        let body_y = 1 + 2;
        let Some(anchor) = d.selection_target_at(10, body_y, area) else {
            panic!("the body band must be hit-testable in the tabbed offer");
        };
        d.begin_selection(anchor, area.width.saturating_sub(5));
        let Some(end) = d.selection_target_at(20, body_y, area) else {
            panic!("the drag end must be hit-testable too");
        };
        d.extend_selection(end);
        assert!(d.has_selection(), "fixture needs a live selection");

        d.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(
            !d.has_selection(),
            "a tab switch must drop the old tab's offsets, not copy the new tab's text"
        );
    }

    /// Single-content mode (no tab bar) keeps the old arrow map: Left/Right
    /// still drive Yes/No — the split only exists when tabs exist.
    #[test]
    fn single_content_mode_keeps_the_old_arrow_map() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show(&SAMPLE);
        d.phase = Phase::WaitingForResponse;
        assert!(!d.has_tabs());

        d.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(d.selected, 1, "Right still selects No without tabs");
        d.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(d.selected, 0, "Left still selects Yes without tabs");
        d.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(d.selected, 1, "Down also drives the column in single mode");
    }

    /// Mouse/keyboard parity: a click on a tab chip switches tabs DURING
    /// the Streaming phase too — the tab bar is chrome in every navigable
    /// phase, and the shared body makes the switch seamless.
    #[test]
    fn mouse_click_switches_tabs_mid_stream() {
        use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

        let mut d = FreeGatewayRecommendationDialog::new();
        d.show_offers(OFFERS);
        d.stream_chars = 30; // mid-stream reveal
        d.phase = Phase::Streaming;

        let area = Rect::new(0, 0, 80, 24);
        // The second tab's chip: first inner row, after " Charm " + gap.
        let tab_x = 3 + format!(" {} ", OFFERS[0].content.tab_label).chars().count() as u16 + 1;
        let click = MouseEvent::new(
            MouseEventType::Up,
            MouseButton::Left,
            tab_x,
            area.y + 1,
            MouseModifiers::none(),
        );
        assert!(
            d.handle_mouse(&click, area, &test_theme()),
            "the tab chip must claim its click"
        );
        assert_eq!(
            d.active_offer().unwrap().provider,
            "copilot",
            "a mid-stream click on the GitHub chip must switch tabs"
        );
        assert_eq!(d.stream_chars, 30, "the shared body must not rewind");
        assert!(d.visible, "a tab click is navigation, never dismissal");
    }

    /// REGRESSION (geometry drift): the question row sits at footer-5 — a
    /// regression moving it would otherwise go uncaught because the
    /// hit-test clamps out-of-band rows back into the options. The click
    /// ON the real row is swallowed (no action), a row BELOW it (the gap)
    /// is not claimed by the options either.
    #[test]
    fn the_question_row_is_hit_tested_at_footer_minus_five() {
        let mut d = FreeGatewayRecommendationDialog::new();
        d.show_offers(OFFERS);
        d.phase = Phase::WaitingForResponse;

        let area = Rect::new(0, 0, 80, 24);
        let footer_y = area.y + d.required_height(area.width).min(area.height) - 2;
        let question_y = footer_y - 5;

        // The exact question row: inside the box, no action fires…
        let click = cosh_tui::core::types::MouseEvent::new(
            cosh_tui::core::types::MouseEventType::Up,
            cosh_tui::core::types::MouseButton::Left,
            10,
            question_y,
            cosh_tui::core::types::MouseModifiers::none(),
        );
        assert!(d.handle_mouse(&click, area, &test_theme()));
        assert!(!d.submitted, "the question row never submits");
        assert!(d.visible, "…and it never dismisses the box either");
    }

    /// The streamed body is ONE generic message shared by every offered
    /// gateway — the tabs (not the prose) carry which gateway is which, so
    /// a mid-stream switch is seamless and the copy mentions both by name.
    #[test]
    fn the_streamed_pitch_is_shared_and_covers_every_offered_gateway() {
        assert!(
            OFFERS.len() >= 2,
            "the tabbed offer needs at least two gateways"
        );
        let pitch = OFFERS[0].content.stream_text;
        for offer in OFFERS {
            assert_eq!(
                offer.content.stream_text, pitch,
                "every tab shares the same body — a switch must never restart the reveal"
            );
            assert!(
                !offer.content.tab_label.is_empty(),
                "every tab needs a label for the tab bar"
            );
        }
        for name in ["Charm", "GitHub Copilot"] {
            assert!(
                pitch.contains(name),
                "the generic pitch must cover {name} by name"
            );
        }
        for pair in OFFERS.windows(2) {
            assert_ne!(
                pair[0].provider, pair[1].provider,
                "two tabs must never dispatch the same provider"
            );
        }
    }
}
