//! The workflow (an org chart of agents, edited visually) and the sessions (one execution of it for
//! a task the user gives). Stored under the app data dir: `workflows/<id>.json` (name, workflow and
//! instructions; there can be several, a session picks one) and `sessions/<sid>.json`.
//!
//! The workflow does not describe a sequence of steps. Its nodes are agents (roles) and an edge
//! `A -> B` means A may hand work to B. The one node nobody hands work to is the root: it gets the
//! user's request. An agent with outgoing edges coordinates the agents it may call: it decides
//! what to delegate, in what order, how often (loops and "for each" included) and when it is done;
//! an agent without them just does its work. The engine (`engine.rs`) enforces the edges.

pub mod analytics;
pub mod commands;
pub mod cron;
pub mod engine;
pub mod model;
pub mod store;
pub mod triggers;
pub mod webhook;

pub use commands::{provision_defaults, root};
pub use model::*;
pub use store::*;
