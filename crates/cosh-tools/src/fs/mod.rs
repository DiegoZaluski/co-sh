// pub mod edit;
pub mod fs_guard;
pub mod read;
// #[cfg(test)]
// mod test;
pub mod types;
pub mod write;

// pub use edit::edit;
pub use read::read;
pub use types::{FsMetadata, ReadFile, Target, TargetFile, WriteAllFile};
