//! What GitLab and GitHub have in common: the domain models the app works with, the error
//! type, and helpers for diffs and job logs. Each backend (`crate::gitlab`, later
//! `crate::github`) converts its API responses into these types.

mod client;
pub mod diff;
mod error;
mod models;
pub mod trace;

pub use client::{Forge, ForgeKind};
pub use error::ApiError;
pub use models::{
    Approvals, Branch, BranchPage, CiAction, CiSnapshot, Comparison, Discussion, FileDiff, Job,
    MergeRequest, MrAction, MrDetail, MrList, MrPage, Note, NotePosition, Pipeline, PipelineList,
    PipelinePage, PipelineStatus, PipelineSummary, ProjectId, Repository, User, sort_branches,
    sort_jobs,
};
