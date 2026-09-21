//! `computer_screenshot` — capture the screen (or a region / element) and deliver
//! the PNG inline as base64 through the connector's multimodal channel.
//!
//! Every xa11y capture is blocking (pixel copy from the compositor), so it
//! runs on tokio's blocking pool. Encoding to PNG happens on the same
//! blocking task — a full-screen RGBA buffer is large and the encode is
//! CPU-bound.
use cosh_sdk::connector::{ChatMessage, ImageBlock, tool_result_message_with_images};
use xa11y::{
    App, AppExt, Rect, screenshot as xa11y_screenshot, screenshot_element,
    screenshot_region as xa11y_screenshot_region,
};

use super::snapshot::DEFAULT_TIMEOUT_MS;
use super::types::{ScreenshotOutput, ComputerScreenshot};

/// Default maximum width of the delivered image, in pixels.
///
/// Coordinates travel in the DELIVERED image's pixel space, so the output
/// always reports the final dimensions and the touch tool maps them back
/// to desktop coordinates using the reported `desktop_origin` and
/// `desktop_scale`. The cap keeps the base64 payload inside a tool-result
/// budget on HiDPI displays (a 4K+ Retina capture would otherwise exceed
/// it).
const DEFAULT_MAX_WIDTH: u32 = 1568;

/// Capture the screen and deliver the PNG inline (base64) for multimodal
/// models.
///
/// Three mutually exclusive modes: full display (no arguments), an explicit
/// display `region` (`[x, y, width, height]` in desktop pixels), or an
/// element capture (`app`/`pid` + `selector` — the pixels under the
/// matched element's current bounds; no auto-raise, so an occluded element
/// yields whatever pixels sit at those coordinates). Oversized captures
/// are downscaled to `max_width` (default 1568), preserving aspect ratio;
/// the output reports the DELIVERED dimensions plus the desktop-space
/// origin and scale so coordinate-based touch can map image points back
/// to the screen.
///
/// # Errors
///
/// Returns `Err` for invalid input (`region` with the wrong arity, element
/// capture without `selector`, `nth` of 0), when the app or selector does
/// not resolve, or when capture/encode fails (e.g. missing screen-recording
/// permission).
pub async fn screenshot(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    if let Some(region) = &input.region {
        if region.len() != 4 {
            return Err(format!(
                "computer_screenshot: `region` takes exactly 4 numbers [x, y, width, height], got {}",
                region.len()
            ));
        }
        if input.app.is_some() || input.pid.is_some() || input.selector.is_some() {
            return Err(
                "computer_screenshot: use `region` OR element capture (`app`/`pid` + `selector`), not both"
                    .into(),
            );
        }
    }
    // Element capture REQUIRES a selector — `app`/`pid` alone must not
    // silently fall back to a full-display capture (scope leak: the model
    // asked for one app and would receive pixels of everything on screen).
    if input.selector.is_none() && (input.app.is_some() || input.pid.is_some()) {
        return Err(
            "computer_screenshot: element capture requires `selector` together with `app` or `pid`; \
             omit all three to capture the full display"
                .into(),
        );
    }
    if input.selector.is_some() && input.app.is_none() && input.pid.is_none() {
        return Err(
            "computer_screenshot: element capture requires `app` or `pid` together with `selector`".into(),
        );
    }
    if input.nth == Some(0) {
        return Err("computer_screenshot: `nth` is 1-based; use 1 for the first match".into());
    }
    if input.nth.is_some() && input.selector.is_none() {
        return Err("computer_screenshot: `nth` requires `selector`".into());
    }

    let input = input.clone();
    tokio::task::spawn_blocking(move || screenshot_blocking(&input))
        .await
        .map_err(|e| format!("computer_screenshot: blocking task failed: {e}"))?
}

fn screenshot_blocking(input: &ComputerScreenshot) -> Result<ScreenshotOutput, String> {
    let capture = match (&input.region, &input.selector) {
        // Explicit region of the display.
        (Some(region), _) => {
            let rect = Rect {
                x: region[0],
                y: region[1],
                width: u32::try_from(region[2])
                    .map_err(|_| "computer_screenshot: region width must be >= 0".to_string())?,
                height: u32::try_from(region[3])
                    .map_err(|_| "computer_screenshot: region height must be >= 0".to_string())?,
            };
            xa11y_screenshot_region(rect)
        }
        // Element capture: resolve app, match the selector, shoot the
        // element's current bounds.
        (None, Some(selector)) => {
            let app = resolve_app(input)?;
            let nth = input.nth.unwrap_or(1);
            let element = app
                .locator(selector)
                .nth(nth)
                .element()
                .map_err(|e| format!("computer_screenshot: selector: {e}"))?;
            screenshot_element(&element)
        }
        // Full display.
        (None, None) => xa11y_screenshot(),
    }
    .map_err(|e| format!("computer_screenshot: capture: {e}"))?;

    // Desktop-space rectangle the capture covers, BEFORE any downscale:
    // the origin the image starts at on screen and how many desktop pixels
    // one image pixel spans. xa11y keeps this mapping through resize, so
    // prefer it; fall back to deriving it from the scale factor. The
    // computer_pointer tool uses origin+scale to map image coordinates
    // back to screen positions.
    let (origin, desktop_scale) = if capture.mapping_available() {
        let origin = capture
            .image_to_desktop(xa11y::Point::new(0, 0))
            .map_err(|e| format!("computer_screenshot: mapping: {e}"))?;
        let corner = capture
            .image_to_desktop(xa11y::Point::new(
                i32::try_from(capture.width.saturating_sub(1)).unwrap_or(i32::MAX),
                i32::try_from(capture.height.saturating_sub(1)).unwrap_or(i32::MAX),
            ))
            .map_err(|e| format!("computer_screenshot: mapping: {e}"))?;
        // Pixel-center mapping: the span between the centers of the first
        // and last image pixel covers (dim-1) pixels, so divide by dim-1 —
        // dividing by dim would systematically underestimate the scale.
        let dx = (corner.x - origin.x).abs().max(1);
        let dy = (corner.y - origin.y).abs().max(1);
        (
            (origin.x, origin.y),
            (
                f64::from(dx) / f64::from(capture.width.saturating_sub(1).max(1)),
                f64::from(dy) / f64::from(capture.height.saturating_sub(1).max(1)),
            ),
        )
    } else {
        ((0, 0), (f64::from(capture.scale), f64::from(capture.scale)))
    };

    // Downscale oversized captures so the base64 payload stays inside a
    // tool-result budget. xa11y's resize preserves the desktop mapping, so
    // re-derive origin/scale from the FINAL image below.
    let max_width = input.max_width.unwrap_or(DEFAULT_MAX_WIDTH).max(256);
    let capture = if capture.width > max_width {
        let target_height = ((f64::from(capture.height) * f64::from(max_width)
            / f64::from(capture.width))
            .round() as u32)
            .max(1);
        capture
            .resize(max_width, target_height)
            .map_err(|e| format!("computer_screenshot: resize: {e}"))?
    } else {
        capture
    };

    // Re-derive the desktop transform from the FINAL delivered image so
    // computer_pointer can map image pixels back to screen coordinates
    // exactly.
    let (origin, desktop_scale) = if capture.mapping_available() {
        let origin = capture
            .image_to_desktop(xa11y::Point::new(0, 0))
            .map_err(|e| format!("computer_screenshot: mapping: {e}"))?;
        let corner = capture
            .image_to_desktop(xa11y::Point::new(
                i32::try_from(capture.width.saturating_sub(1)).unwrap_or(i32::MAX),
                i32::try_from(capture.height.saturating_sub(1)).unwrap_or(i32::MAX),
            ))
            .map_err(|e| format!("computer_screenshot: mapping: {e}"))?;
        // Pixel-center mapping: the span between the centers of the first
        // and last image pixel covers (dim-1) pixels, so divide by dim-1 —
        // dividing by dim would systematically underestimate the scale.
        let dx = (corner.x - origin.x).abs().max(1);
        let dy = (corner.y - origin.y).abs().max(1);
        (
            (origin.x, origin.y),
            (
                f64::from(dx) / f64::from(capture.width.saturating_sub(1).max(1)),
                f64::from(dy) / f64::from(capture.height.saturating_sub(1).max(1)),
            ),
        )
    } else {
        (origin, desktop_scale)
    };

    let png = capture
        .to_png()
        .map_err(|e| format!("computer_screenshot: encode: {e}"))?;
    let image = ImageBlock::png(&png);

    Ok(ScreenshotOutput {
        width: capture.width,
        height: capture.height,
        desktop_origin: origin,
        desktop_scale,
        bytes: png.len(),
        images: vec![image],
    })
}

/// Resolve the target application (validating `app`/`pid` exclusivity).
fn resolve_app(input: &ComputerScreenshot) -> Result<App, String> {
    use std::time::Duration;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let name = input.app.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (name, input.pid) {
        (Some(name), None) => App::by_name(name, timeout)
            .map_err(|e| format!("computer_screenshot: resolve application: {e}")),
        (None, Some(pid)) => App::by_pid(pid, timeout)
            .map_err(|e| format!("computer_screenshot: resolve application: {e}")),
        (None, None) => Err("computer_screenshot: element capture requires `app` or `pid`".into()),
        (Some(_), Some(_)) => Err("computer_screenshot: provide `app` or `pid`, not both".into()),
    }
}

/// Build the connector message for a screenshot tool result: short text
/// plus the inline PNG (the harness replays this in the conversation so a
/// multimodal model sees the image).
#[must_use]
pub fn screenshot_tool_result(tool_call_id: &str, output: &ScreenshotOutput) -> ChatMessage {
    tool_result_message_with_images(
        tool_call_id,
        &format!(
            "screenshot {}x{} px ({} bytes base64 PNG), covering desktop pixels \
             from ({}, {}) at {:.3}x{:.3} desktop px per image px (dx, dy). \
             Pointer coordinates are in THIS image's pixel space; map to the \
             display as desktop = origin + image_coord * scale.",
            output.width,
            output.height,
            output.bytes,
            output.desktop_origin.0,
            output.desktop_origin.1,
            output.desktop_scale.0,
            output.desktop_scale.1,
        ),
        output.images.clone(),
    )
}
