//! GitLab API client. Converts GitLab's REST and GraphQL responses into the shared models of
//! `crate::forge`; knows nothing about the UI.

mod client;
mod graphql;
mod rest;

pub use client::GitLabClient;
