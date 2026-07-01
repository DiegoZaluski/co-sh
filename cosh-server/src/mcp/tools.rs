use cosh_tools::{
    bash::Bash,
    fs::{Fs, FsMetadata, FsRead, FsWrite},
    plan::Plan,
    vision::Vision,
    web::Web,
};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[allow(dead_code)]
struct Server {
    fs: Fs,
    web: Web,
    plan: Plan,
    bash: Bash,
    vision: Vision,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ParametersFsRead {
    metadata: FsMetadata,
    targets: FsRead,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct ParametersFsWrite {
    metadata: FsMetadata,
    targets: FsWrite,
}

#[allow(dead_code)]
#[tool_router]
impl Server {
    pub fn new() -> Self {
        Self {
            fs: Fs::new(),
            web: Web::new(),
            plan: Plan::new(),
            bash: Bash::new(),
            vision: Vision::new(),
        }
    }

    // ............................... TOOLS SECTION ...

    // --- FILESYSTEM ---

    #[tool]
    pub async fn fs_read(
        &self,
        Parameters(params): Parameters<ParametersFsRead>,
    ) -> Result<CallToolResult, ErrorData> {
        let results = self.fs.read(params.targets.targets).await;
        Ok(CallToolResult::success(
            results
                .into_iter()
                .map(|r| Content::text(serde_json::to_string(&r).unwrap_or_default()))
                .collect(),
        ))
    }

    #[tool]
    pub async fn fs_write(
        &self,
        Parameters(params): Parameters<ParametersFsWrite>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.fs.write(params.targets.targets).await {
            Ok(results) => Ok(CallToolResult::success(
                results
                    .into_iter()
                    .map(|r| Content::text(serde_json::to_string(&r).unwrap_or_default()))
                    .collect(),
            )),
            Err(err) => Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, err, None)),
        }
    }

    #[tool]
    pub fn fs_edit() {
        todo!()
    }

    // --- WEB ---

    #[tool]
    pub fn web_search() {
        todo!()
    }

    #[tool]
    pub fn web_fetch() {
        todo!()
    }

    // --- BASH ---

    #[tool]
    pub fn bash_exec() {
        todo!()
    }

    // --- PLAN ---

    #[tool]
    pub fn todo_read() {
        todo!()
    }

    #[tool]
    pub fn todo_write(&self) {
        todo!()
    }

    #[tool]
    pub fn todo_edit() {
        todo!()
    }

    #[tool]
    pub fn todo_cross_off() {
        todo!()
    }

    // --- VISION ---

    #[tool]
    fn vision_bash(&self) {}
}
