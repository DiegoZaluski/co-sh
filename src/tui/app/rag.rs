use cosh_tui::core::types::MouseEvent;
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

use super::App;
#[cfg(feature = "embed")]
use super::AppMode;
#[cfg(feature = "embed")]
use crate::ui::dialogs::DialogType;

impl App {
    #[cfg(feature = "embed")]
    pub(super) fn is_rag_mode(&self) -> bool {
        matches!(self.mode(), AppMode::Rag)
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn is_rag_mode(&self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn rag_spinner_active(&self) -> bool {
        self.rag_view.is_spinner_active()
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn rag_spinner_active(&self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn render_rag_view(
        &mut self,
        buf: &mut ratatui::buffer::Buffer,
        session_area: Rect,
    ) {
        self.prompt_view.blur();
        self.rag_view.advance_spinner();
        // Show toast when embed completes
        if self.rag_view.take_embed_completed() {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("RAG".into()),
                message: "Content embedded successfully.".into(),
                variant: ToastVariant::Success,
                duration_ms: 4000,
            });
        }
        if let Some(err) = self.rag_view.take_embed_error() {
            use crate::ui::toast::{ToastOptions, ToastVariant};
            self.toast_state.show(ToastOptions {
                title: Some("RAG".into()),
                message: err,
                variant: ToastVariant::Error,
                duration_ms: 6000,
            });
        }
        let tools_area = Rect::new(
            session_area.x,
            session_area.y,
            session_area.width,
            session_area.height.saturating_sub(1),
        );
        self.rag_view.render(buf, tools_area, &self.theme);
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn render_rag_view(
        &mut self,
        _buf: &mut ratatui::buffer::Buffer,
        _session_area: Rect,
    ) {
    }

    #[cfg(feature = "embed")]
    pub(super) fn handle_rag_confirm_delete(&mut self) -> bool {
        if let Some(db_name) = self.pending_delete_db_name.take() {
            self.rag_view.registry.remove(&db_name);
            self.rag_view.active_dbs.remove(&db_name);
            crate::routes::rag::registry::RagRegistry::save_active(&self.rag_view.active_dbs);
            if self.rag_view.selected_db_for_embed.as_deref() == Some(&db_name) {
                self.rag_view.selected_db_for_embed = None;
            }
            self.dialog.pop();
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn handle_rag_confirm_delete(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn clear_rag_pending_state(&mut self) {
        self.pending_delete_db_name = None;
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn clear_rag_pending_state(&mut self) {}

    #[cfg(feature = "embed")]
    pub(super) fn handle_rag_cancel_action(&mut self) {
        if matches!(self.mode(), AppMode::Rag) {
            self.show_rag = false;
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn handle_rag_cancel_action(&mut self) {}

    #[cfg(feature = "embed")]
    pub(super) fn recall_suffix(&self) -> String {
        self.rag_view
            .registry
            .build_tool_suffix(self.rag_view.active_dbs())
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn recall_suffix(&self) -> String {
        String::new()
    }

    #[cfg(feature = "embed")]
    pub(super) fn maybe_disable_recall_tool(
        &self,
        disabled: &mut std::collections::HashSet<String>,
    ) {
        if self.rag_view.registry.dbs.is_empty() {
            disabled.insert("recall_search".into());
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn maybe_disable_recall_tool(
        &self,
        _disabled: &mut std::collections::HashSet<String>,
    ) {
    }

    #[cfg(feature = "embed")]
    pub(super) fn recall_dbs_vec(&self) -> Vec<cosh::harness::tools::RecallDb> {
        use cosh::harness::tools::{RecallDb, RecallEmbedderConfig};
        self.rag_view
            .registry
            .dbs
            .iter()
            .filter(|db| self.rag_view.active_dbs.contains(&db.name))
            .map(|db| RecallDb {
                name: db.name.clone(),
                uri: db.uri.clone(),
                table_name: db.name.clone(),
                embedder: match &db.embedder {
                    crate::routes::rag::models::EmbedderConfig::Local { model } => {
                        RecallEmbedderConfig::Local {
                            model_name: serde_json::to_value(model)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default(),
                        }
                    }
                    crate::routes::rag::models::EmbedderConfig::Cloud(c) => {
                        RecallEmbedderConfig::Cloud {
                            provider: c.provider.clone(),
                            model: c.model.clone(),
                            dim: db.embedder.vector_dim(),
                        }
                    }
                },
            })
            .collect()
    }

    #[cfg(feature = "embed")]
    pub(super) fn handle_rag_key_event(&mut self, key: KeyCode) -> bool {
        if self.is_rag_mode() && !self.dialog.visible() {
            use crate::routes::rag::RagAction;
            match self.rag_view.handle_key(key) {
                Some(RagAction::Back) => {
                    // Abort any in-flight async operations so the app doesn't
                    // accumulate stale background tasks after leaving RAG mode.
                    if let Some(h) = self.rag_fetch_handle.take() {
                        h.abort();
                        log::debug!("[tui_rag_app] Aborted fetch task on Esc");
                    }
                    if let Some(h) = self.rag_embed_handle.take() {
                        h.abort();
                        log::debug!("[tui_rag_app] Aborted embed task on Esc");
                    }
                    self.rag_view.fetch_rx = None;
                    self.rag_view.embed_rx = None;
                    self.show_rag = false;
                }
                Some(RagAction::ClosePreview) => {}
                Some(RagAction::FetchUrlOrPath(input)) => {
                    self.rag_view.start_fetch(&input);
                    if input.starts_with("http://") || input.starts_with("https://") {
                        let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
                        let url = input.clone();
                        let url_for_log = url.clone();
                        let handle = self.tokio_handle.spawn(async move {
                            use cosh_tools::web::{WebFetch, fetch as web_fetch_fn};
                            let fetch_input = WebFetch { url };
                            let result = web_fetch_fn(&fetch_input).await;
                            let _ = tx.send(result);
                        });
                        self.rag_fetch_handle = Some(handle);
                        log::debug!("[tui_rag_app] Spawned fetch task for URL: {url_for_log}");
                        self.rag_view.set_fetch_rx(rx);
                    } else {
                        match std::fs::read_to_string(&input) {
                            Ok(content) => self.rag_view.content_fetched(content),
                            Err(_) => self.rag_view.set_error(),
                        }
                    }
                }
                Some(RagAction::CreateDb {
                    name,
                    description,
                    embedder,
                }) => {
                    use crate::routes::rag::models::RagDb;
                    let db = RagDb {
                        name: name.clone(),
                        uri: crate::routes::rag::registry::RagRegistry::db_uri(&name),
                        description: description.clone(),
                        embedder,
                        created_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    };
                    let mut registry = crate::routes::rag::registry::RagRegistry::load();
                    registry.upsert(db);
                    self.rag_view.selected_db_for_embed = Some(name.clone());
                    self.rag_view.active_dbs.insert(name.clone());
                    crate::routes::rag::registry::RagRegistry::save_active(
                        &self.rag_view.active_dbs,
                    );
                    self.rag_view.registry = crate::routes::rag::registry::RagRegistry::load();
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("Database Created".into()),
                        message: format!("'{name}' is now selected for embedding."),
                        variant: ToastVariant::Success,
                        duration_ms: 4000,
                    });
                }
                Some(RagAction::ShowWarning(msg)) => {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    self.toast_state.show(ToastOptions {
                        title: Some("RAG".into()),
                        message: msg,
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                }
                Some(RagAction::EmbedContent {
                    content,
                    db_name,
                    db_description: _,
                    model: _,
                }) => {
                    // Look up the DB from the registry to get its URI and embedder config
                    let db_entry = self.rag_view.registry.find(&db_name).cloned();
                    if let Some(db) = db_entry {
                        self.rag_view.start_embedding();
                        let uri = db.uri.clone();
                        let table_name = db.name.clone();
                        let embedder_config = db.embedder.clone();
                        let embed_content = content.clone();
                        let base_urls = self.configured_local_base_urls();
                        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
                        let handle = self.tokio_handle.spawn(async move {
                            let result = embed_document(
                                &uri,
                                &table_name,
                                &embedder_config,
                                &embed_content,
                                &base_urls,
                            )
                            .await;
                            let _ = tx.send(result);
                        });
                        self.rag_embed_handle = Some(handle);
                        log::debug!("[tui_rag_app] Spawned embed task for DB: {db_name}");
                        self.rag_view.set_embed_rx(rx);
                    } else {
                        use crate::ui::toast::{ToastOptions, ToastVariant};
                        self.toast_state.show(ToastOptions {
                            title: Some("RAG".into()),
                            message: format!("Database '{db_name}' not found."),
                            variant: ToastVariant::Error,
                            duration_ms: 4000,
                        });
                    }
                }
                _ => {}
            }
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn handle_rag_key_event(&mut self, _key: KeyCode) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn handle_rag_paste(&mut self, text: &str) {
        self.rag_view.handle_paste(text);
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn handle_rag_paste(&mut self, _text: &str) {}

    #[cfg(feature = "embed")]
    pub(super) fn try_rag_scroll_up(&mut self) -> bool {
        if !self.dialog.visible() {
            self.rag_view.handle_key(KeyCode::Up);
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn try_rag_scroll_up(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn try_rag_scroll_down(&mut self) -> bool {
        if !self.dialog.visible() {
            self.rag_view.handle_key(KeyCode::Down);
            true
        } else {
            false
        }
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn try_rag_scroll_down(&mut self) -> bool {
        false
    }

    #[cfg(feature = "embed")]
    pub(super) fn handle_rag_mouse_click(&mut self, mouse: &MouseEvent) -> bool {
        let area = self.terminal_size();
        let main_area = self.main_content_area(area);
        let tools_area = Rect::new(
            main_area.x,
            area.y + 1,
            main_area.width,
            main_area.height.saturating_sub(4),
        );
        // DB picker row click (select DB for embed)
        if self.rag_view.show_db_picker {
            if let Some(picker_idx) = self.rag_view.is_db_picker_row_click(mouse, tools_area) {
                let pick_name = self
                    .rag_view
                    .filtered_dbs()
                    .get(picker_idx)
                    .map(|db| db.name.clone());
                if let Some(ref name) = pick_name {
                    self.rag_view.select_db_for_embed(name);
                }
                return true;
            }
            if !self.rag_view.is_click_inside_db_picker(mouse, tools_area) {
                self.rag_view.close_db_picker();
                return true;
            }
            return true;
        }
        // "Select Database" button click
        if self.rag_view.is_select_db_click(mouse, tools_area) {
            self.rag_view.toggle_db_picker();
            return true;
        }
        // "show desc" button click
        if let Some(desc_idx) = self.rag_view.is_show_desc_click(mouse, tools_area) {
            self.rag_view.toggle_desc_popup(desc_idx);
            return true;
        }
        // delete button click
        if let Some(db_name) = self.rag_view.is_delete_click(mouse, tools_area) {
            self.pending_delete_db_name = Some(db_name);
            self.dialog.show(DialogType::Confirm {
                message: "Delete this database?".into(),
            });
            if let Some(d) = self.dialog.current_mut() {
                d.selected = 1;
            }
            return true;
        }
        // toggle DB active
        if let Some(clicked_idx) = self.rag_view.handle_mouse(mouse, tools_area) {
            self.rag_view.toggle_db(clicked_idx);
            return true;
        }
        // Create DB button
        if self.rag_view.is_create_click(mouse, tools_area) {
            self.rag_view.toggle_create_db();
            return true;
        }
        // Model line toggle
        if self.rag_view.is_model_click(mouse, tools_area) {
            self.rag_view.toggle_models_expanded();
            return true;
        }
        // Click outside preview
        if self.rag_view.is_preview_visible()
            && self.rag_view.is_click_outside_preview(mouse, tools_area)
        {
            self.rag_view.close_preview();
            return true;
        }
        // Click outside description popup
        if self.rag_view.show_desc_for_db.is_some()
            && self.rag_view.is_click_outside_desc_popup(mouse, tools_area)
        {
            self.rag_view.close_desc_popup();
            return true;
        }
        // Create DB form field click
        if self.rag_view.show_create_db
            && self
                .rag_view
                .handle_create_db_field_click(mouse, tools_area)
        {
            return true;
        }
        // Dismiss Create DB form
        if self.rag_view.is_dismiss_click(mouse, tools_area) {
            self.rag_view.close_form();
            return true;
        }
        // Click on the URL input box → position cursor at the click location
        {
            let inner_w = tools_area.width.saturating_sub(4);
            let input_w = inner_w.saturating_sub(4);
            let input_h = self.rag_view.url_input.height(input_w);
            let input_x = tools_area.x + 4; // = layout.cx
            let input_y = tools_area.y + 2;
            let mx = mouse.x;
            let my = mouse.y;
            if my >= input_y && my < input_y + input_h && mx >= input_x && mx < input_x + input_w {
                self.rag_view.url_input.focus();
                if let Some(pos) = self.rag_view.url_input.char_pos_at_mouse(
                    mx,
                    my,
                    Rect::new(input_x, input_y, input_w, input_h),
                ) {
                    self.rag_view.url_input.cursor_pos = pos;
                    self.rag_view.url_input.cursor.note_activity();
                }
                return true;
            }
        }

        false
    }

    #[cfg(not(feature = "embed"))]
    pub(super) fn handle_rag_mouse_click(&mut self, _mouse: &MouseEvent) -> bool {
        false
    }
}

#[cfg(feature = "embed")]
pub(super) async fn embed_document(
    uri: &str,
    table_name: &str,
    embedder_config: &crate::routes::rag::models::EmbedderConfig,
    content: &str,
    base_urls: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    use cosh_recall::embed::Rag;

    let dim = embedder_config.vector_dim();
    let embedder = match embedder_config {
        crate::routes::rag::models::EmbedderConfig::Local { model } => {
            create_local_embedder(*model)?
        }
        crate::routes::rag::models::EmbedderConfig::Cloud(c) => {
            create_cloud_embedder(&c.provider, &c.model, dim, base_urls)?
        }
    };

    let rag = Rag::connect(uri, table_name, embedder)
        .await
        .map_err(|e| format!("Failed to connect to database: {e}"))?;

    rag.ingest("content", content)
        .await
        .map_err(|e| format!("Failed to embed content: {e}"))?;

    Ok(())
}

#[cfg(feature = "embed")]
pub(super) fn create_local_embedder(
    model: crate::routes::rag::models::LocalEmbedModel,
) -> Result<cosh_recall::embed::Embedder, String> {
    use cosh_recall::embed::Embedder;
    let fast_model = model.to_fastembed_model();
    Embedder::try_new_local(fast_model).map_err(|e| format!("Failed to create local embedder: {e}"))
}

#[cfg(all(feature = "embed", feature = "cloud"))]
pub(super) fn create_cloud_embedder(
    provider: &str,
    model: &str,
    dim: usize,
    base_urls: &std::collections::HashMap<String, String>,
) -> Result<cosh_recall::embed::Embedder, String> {
    use cosh_recall::embed::Embedder;
    use cosh_sdk::connector::Connector;
    let mut connector = Connector::new(provider)
        .map_err(|e| format!("Failed to create connector: {e}"))?
        .with_model(model);
    if let Some(url) = base_urls.get(provider) {
        connector = connector.with_base_url(url.clone());
    }
    Ok(Embedder::new_cloud(connector, dim))
}

#[cfg(all(feature = "embed", not(feature = "cloud")))]
pub(super) fn create_cloud_embedder(
    _provider: &str,
    _model: &str,
    _dim: usize,
    _base_urls: &std::collections::HashMap<String, String>,
) -> Result<cosh_recall::embed::Embedder, String> {
    Err("Cloud embedding requires the 'cloud' feature (enable with --features cloud)".into())
}
