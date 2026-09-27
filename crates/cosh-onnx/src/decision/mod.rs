//! The decision contract: the question protocol, confidence estimation, the
//! schema mapping, the sequence builder and the model trait — the crate-level
//! core every decision backend implements.
//!
//! The question protocol (`choice` / `score` / `noul`) is a crate-level
//! contract, not a laya detail: laya is its first implementation, and a
//! future model that speaks the same protocol reuses this module unchanged.

pub mod confidence;
pub mod model;
pub mod question;
pub mod schema;
pub mod sequence;

#[cfg(test)]
mod tests;
