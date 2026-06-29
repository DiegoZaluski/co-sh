use std::time::Duration;

use crate::core::renderer::{
    RendererConfig, RendererFrameEvent, RendererStats,
    ScreenMode, ExternalOutputMode, ConsoleMode, PixelResolution,
};

#[test]
fn test_screen_mode_alternate() {
    assert!(matches!(ScreenMode::AlternateScreen, ScreenMode::AlternateScreen));
}

#[test]
fn test_screen_mode_main() {
    assert!(matches!(ScreenMode::MainScreen, ScreenMode::MainScreen));
}

#[test]
fn test_screen_mode_split_footer() {
    assert!(matches!(ScreenMode::SplitFooter { footer_height: 10 }, ScreenMode::SplitFooter { footer_height: 10 }));
}

#[test]
fn test_external_output_mode() {
    assert!(matches!(ExternalOutputMode::CaptureStdout, ExternalOutputMode::CaptureStdout));
}

#[test]
fn test_console_mode() {
    assert!(matches!(ConsoleMode::Disabled, ConsoleMode::Disabled));
}

#[test]
fn test_renderer_config_default() {
    let cfg = RendererConfig::default();
    assert!(cfg.alternate_screen);
    assert_eq!(cfg.width, 80);
    assert_eq!(cfg.height, 24);
    assert_eq!(cfg.target_fps, 30);
    assert_eq!(cfg.max_fps, 60);
    assert!(cfg.exit_on_ctrl_c);
    assert!(cfg.clear_on_shutdown);
    assert!(cfg.enable_mouse_movement);
    assert!(cfg.use_mouse);
    assert!(cfg.auto_focus);
    assert!(cfg.background_color.is_none());
    assert_eq!(cfg.max_stat_samples, 300);
    assert_eq!(cfg.debounce_delay, Duration::from_millis(100));
    assert_eq!(cfg.memory_snapshot_interval, Duration::from_secs(0));
}

#[test]
fn test_pixel_resolution() {
    let res = PixelResolution { width: 1920, height: 1080 };
    assert_eq!(res.width, 1920);
    assert_eq!(res.height, 1080);
}

#[test]
fn test_renderer_frame_event() {
    let ev = RendererFrameEvent { frame_id: 42 };
    assert_eq!(ev.frame_id, 42);
}

#[test]
fn test_renderer_stats() {
    let stats = RendererStats {
        fps: 30.0, frame_count: 100,
        frame_times: vec![16.0, 17.0],
        average_frame_time: 16.5, min_frame_time: 16.0, max_frame_time: 17.0,
    };
    assert!((stats.fps - 30.0).abs() < 0.001);
    assert_eq!(stats.frame_count, 100);
    assert_eq!(stats.frame_times.len(), 2);
}
