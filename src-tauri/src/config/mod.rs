//! User-managed settings the runs are built from: projects, agent profiles, Claude accounts,
//! secrets, variables, credential injections and the network allowlist. Most exist twice: in the
//! global settings and in each project (`scope.rs`).

pub mod accounts;
pub mod injections;
pub mod integrations;
pub mod network;
pub mod profiles;
pub mod projects;
pub mod scope;
pub mod secrets;
pub mod variables;
pub mod vm_settings;
