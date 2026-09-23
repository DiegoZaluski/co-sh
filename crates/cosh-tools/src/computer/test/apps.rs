//! Tests for the apps tool: focus queries against the mock provider.
#[cfg(test)]
mod apps_focus_tests {
    use std::sync::Arc;

    use xa11y::mock::build_provider;
    use xa11y::{Element, Provider, Role};

    use crate::computer::types::{AppsTarget, ComputerApps};
    use crate::computer::apps::{find_focused, find_focused_below, FOCUSED_WALK_MAX_DEPTH};

    /// The mock fixture's window reports `focused` — the walk below the app
    /// root must find it and report the role path from the root down to the
    /// leaf, inclusive (both snake_case). The root's own (synthetic)
    /// foreground stamp must not match: the answer is the window, not the
    /// application node.
    #[test]
    fn walk_finds_focused_element_with_role_path() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, Arc::clone(&provider));

        let (path, leaf) = find_focused_below(&app_root)
            .expect("the fixture window holds focus, so the walk must find it");
        assert_eq!(path, vec!["application", "window"]);
        assert_eq!(leaf.role, Role::Window);
        assert!(leaf.states.focused);
    }

    /// xa11y's `foreground_with` stamps `states.focused = true` on the app
    /// root it returns (a *foreground-app* marker, not keyboard focus). The
    /// walk below must ignore that stamp: with the root stamped — exactly
    /// what production sees — the answer is still the focused WINDOW, never
    /// the root itself.
    #[test]
    fn walk_ignores_the_roots_synthetic_focus_stamp() {
        let provider: Arc<dyn Provider> = build_provider();
        let mut root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        root_data.states.focused = true; // what foreground_with stamps
        let app_root = Element::new(root_data, provider);

        let (path, leaf) =
            find_focused_below(&app_root).expect("the focused window sits below the root");
        assert_eq!(path, vec!["application", "window"]);
        assert_eq!(leaf.role, Role::Window);
    }

    /// A subtree with no focused element yields `None` — rendered later as
    /// `focused_element: null`, never an error.
    #[test]
    fn walk_returns_none_when_nothing_is_focused() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, provider);
        let window = app_root.children().expect("window child")[0].clone();
        let toolbar = window.children().expect("toolbar child")[0].clone();

        assert!(find_focused(&toolbar, 0).is_none());
    }

    /// The walk honors the depth bound: past the cap it reports absence
    /// instead of walking unbounded trees — here the cap bites before the
    /// (focused) window is even examined.
    #[test]
    fn walk_stops_at_depth_cap() {
        let provider: Arc<dyn Provider> = build_provider();
        let root_data = provider.list_apps().expect("mock exposes an app root")[0].clone();
        let app_root = Element::new(root_data, provider);
        let window = app_root.children().expect("window child")[0].clone();

        assert!(find_focused(&window, FOCUSED_WALK_MAX_DEPTH + 1).is_none());
        // Sanity: the same element at depth 0 IS found — the cap, not the
        // element, is what made the walk stop.
        assert!(find_focused(&window, 0).is_some());
    }

    /// Default target is `all` — an empty input keeps the old listing
    /// behavior, so existing callers passing `{}` are unaffected.
    #[test]
    fn default_target_is_all() {
        let input: ComputerApps = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(input.target.unwrap_or_default(), AppsTarget::All);
        let explicit: ComputerApps =
            serde_json::from_value(serde_json::json!({ "target": "focused" })).unwrap();
        assert_eq!(explicit.target.unwrap_or_default(), AppsTarget::Focused);
    }
}
