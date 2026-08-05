pub mod core;

#[cfg(test)]
mod test;

pub use core::{
    MAX_PATHS, MAX_SNAPSHOT_BYTES, MAX_VERSIONS_PER_PATH, RestoreInput, RestoreOutput, record,
    record_seen_lines, restore, session_store,
};
