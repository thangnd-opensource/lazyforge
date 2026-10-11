//! GitHub API client (github.com and GitHub Enterprise Server). Converts GitHub's REST and
//! GraphQL responses into the shared models of `crate::forge`; knows nothing about the UI.

mod actions;
mod branches;
mod client;
pub mod device;
mod rest;

pub use client::GitHubClient;
