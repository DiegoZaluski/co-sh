//! Ported tests: `tests/test_email.py`.
//!
//! A disclaimer footer must not delete the sender's actual request: the
//! upstream regression notes travel with the tests. `email_questions` is
//! defined once (in `presets`) and re-exported here, so the
//! "one definition behind both module paths" checks reduce to the re-export
//! plus the same-answers checks.

use serde_json::{json, Value};

use super::{clean_email_body, email_state};
use crate::laya::presets::email_questions;

const DISCLAIMER: &str = "This email is confidential and intended solely for the named addressee.";

fn clean(body: &str) -> String {
    clean_email_body(body, 3000)
}
mod brazilian;
mod cleaning;
mod footers;
mod from_header;
mod pt_es;
mod questions;
mod signoff;
mod state;
