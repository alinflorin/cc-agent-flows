//! The workflow engine: runs a session over the workflow's org chart (`workflow.rs`).
//!
//! The root agent gets the user's request. An agent with outgoing edges is a *coordinator*: each
//! time it runs it answers with a structured decision, to `delegate` work to agents it has an
//! edge to (several at once run in parallel), to `finish` (its result goes to whoever called it),
//! or to `ask` (questions it cannot answer: they go to its caller, and from the root to the user).
//! An agent without outgoing edges does its work and reports once. A caller is run again, in a
//! fresh VM, when the agents it called report back: no Claude session is ever resumed, so each
//! prompt is rebuilt from the session goal, what the agent was asked, its own `notes` (progress it
//! keeps for itself, only inside this session) and the results it is handed. The edges are the
//! only allowed moves; the engine rejects a decision that leaves them, and the limits (calls per
//! agent, depth, rounds, runs per session, parallel VMs) hold whatever the agents decide.
//!
//! The user takes part through the root's questions (unless the session's autonomy is `never`)
//! and through edges marked as needing approval.
//!
//! `drive` holds the logic and talks to the outside only through `Host` (running an agent, logging,
//! saving), so tests use a scripted host. `RealHost` boots VMs. Resuming a session after a restart
//! replays it: every agent run is journaled under a key made of the path of the call, and `drive`
//! walks the same decisions again, taking the journaled result instead of starting a VM.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

use crate::config::profiles::Profile;
use crate::workflow::{Node, Workflow};
use crate::vm::protocol::{Final, Outbound, Status, Task};
use crate::vm::runs::{Spec, failed};

/// Agent runs (VMs) one session may start, whatever the per-agent limits say.
const MAX_RUNS: u32 = 300;
/// How deep delegation may go (the root is 0).
const MAX_DEPTH: usize = 8;
/// Turns one coordinator call may take.
const MAX_ROUNDS: u32 = 60;
/// Agents one decision may call at once.
const MAX_PARALLEL_CALLS: usize = 5;
/// VMs running at the same time for one session.
const MAX_VMS: usize = 4;
/// Sessions that may run (or wait for the user) at the same time.
const MAX_SESSIONS: usize = 5;
/// Questions that may reach the user in one run of the session; later ones are refused.
const MAX_USER_QUESTIONS: u32 = 10;
/// Attempts a coordinator gets to give a decision the graph allows.
const DECISION_ATTEMPTS: u32 = 2;
/// How much text the prompts carry.
const LOG_CHARS: usize = 8_000;
const ITEM_CHARS: usize = 6_000;

mod decisions;
mod host;
mod prompts;
mod run;
mod state;

pub use decisions::*;
pub use host::*;
pub use prompts::*;
pub use run::*;
pub use state::*;
