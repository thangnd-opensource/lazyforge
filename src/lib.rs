//! lazyforge as a library. The binary in `main.rs` is a thin wrapper; `gitlab` and `github`
//! are meant to be reusable on their own (for a notifier, an AI reviewer, ...).

pub mod app;
pub mod auth;
pub mod config;
pub mod forge;
pub mod git;
pub mod github;
pub mod gitlab;
pub mod runtime;
pub mod selection;
pub mod ui;
