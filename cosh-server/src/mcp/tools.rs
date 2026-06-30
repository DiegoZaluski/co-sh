use cosh_tool::{
    bash::Bash,
    fs::{Fs, FsMetadata},
    plan::Plan,
    vision::Vision,
    web::Web,
};
use mcp::{ServerHandler, model::*, tools, tools_router};

struct Server {
    fs: Fs,
    fs_metadata: FsMetadata,
    web: Web,
    plan: Plan,
    bash: Bash,
    vision: Vision,
}

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

    // ............................... CONTROL SETTERS...

    fn dir_root(&self) {
        todo!()
    }
    // ............................... TOOLS SECTION ...

    // --- FILESYSTEM ---

    #[tool]
    pub fn fs_read(&self) {
        self.fs.read();
    }

    #[tool]
    pub fn fs_write() {
        todo!()
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
