//! The demo content in one place: `demo/demo.yaml` (the demo project with its variables, secrets,
//! injections, images, profiles and the standard workflow) and the image scripts next to it. The
//! welcome dialog's demo content and the "Reset to demo content" buttons write it; each module's
//! `provision_*` takes its part from here. Parsed once; a mistake in the file panics, which `cargo test` catches.

use crate::vm::images::{Doc, Spec};
use crate::config::injections::Injection;
use crate::config::profiles::{self, Profile};
use crate::config::projects::Project;
use crate::config::scope::Scope;
use crate::workflow::{Edge, EdgeKind, Node, NodeKind, Workflow};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const FILE: &str = include_str!("../demo/demo.yaml");

/// The image scripts the file names (`script:`), compiled in.
fn script(name: &str) -> &'static str {
    match name {
        "manager.sh" => include_str!("../demo/manager.sh"),
        "developer.sh" => include_str!("../demo/developer.sh"),
        "qa.sh" => include_str!("../demo/qa.sh"),
        _ => panic!("demo.yaml: unknown script {name}"),
    }
}

/// A list entry with its id next to the fields of the thing itself.
#[derive(Deserialize)]
struct Item<T> {
    id: String,
    #[serde(flatten)]
    value: T,
}

#[derive(Deserialize)]
struct RawImage {
    name: String,
    #[serde(default)]
    description: String,
    parent: String,
    #[serde(default)]
    env: Vec<profiles::EnvVar>,
    script: String,
    #[serde(default)]
    docs: Vec<Doc>,
}

#[derive(Deserialize)]
struct RawNode {
    id: String,
    #[serde(default)]
    label: String,
    /// `agent` (default) or a trigger such as `chat`.
    #[serde(default)]
    kind: NodeKind,
    #[serde(default)]
    profile: String,
    position: [i32; 2],
    #[serde(default = "one")]
    max_calls: u32,
    #[serde(default)]
    prompt: String,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawEdge {
    from: String,
    to: String,
    #[serde(default)]
    kind: EdgeKind,
}

#[derive(Deserialize)]
struct RawWorkflow {
    id: String,
    name: String,
    nodes: Vec<RawNode>,
    edges: Vec<RawEdge>,
}

#[derive(Deserialize)]
struct RawProject {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    system_prompt: String,
    #[serde(default)]
    allow_hosts: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    project: RawProject,
    variables: BTreeMap<String, String>,
    secrets: Vec<String>,
    injections: Vec<Injection>,
    images: Vec<Item<RawImage>>,
    profiles: Vec<Item<Profile>>,
    workflow: RawWorkflow,
}

/// The standard workflow with the id and name it is saved under.
pub struct DemoWorkflow {
    pub id: String,
    pub name: String,
    pub workflow: Workflow,
}

fn raw() -> &'static Raw {
    static RAW: OnceLock<Raw> = OnceLock::new();
    RAW.get_or_init(|| serde_yaml_ng::from_str(FILE).unwrap_or_else(|e| panic!("demo/demo.yaml: {e}")))
}

/// The id of the demo project.
pub fn project_id() -> &'static str {
    &raw().project.id
}

fn project() -> Project {
    let p = &raw().project;
    Project { name: p.name.clone(), description: p.description.clone(), system_prompt: p.system_prompt.clone(), allow_hosts: p.allow_hosts.clone() }
}

/// The demo variables, name and placeholder value.
pub fn variables() -> Vec<(String, String)> {
    raw().variables.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The names of the demo secrets (written with a dummy value).
pub fn secrets() -> &'static [String] {
    &raw().secrets
}

pub fn injections() -> Vec<Injection> {
    raw().injections.clone()
}

/// The starter images, parents first, as (id, spec).
pub fn images() -> Vec<(String, Spec)> {
    raw()
        .images
        .iter()
        .map(|i| {
            let v = &i.value;
            let spec = Spec { name: v.name.clone(), description: v.description.clone(), parent: v.parent.clone(), env: v.env.clone(), script: script(&v.script).into(), docs: v.docs.clone() };
            (i.id.clone(), spec)
        })
        .collect()
}

/// The starter profiles, as (id, profile).
pub fn profiles() -> Vec<(String, Profile)> {
    raw().profiles.iter().map(|p| (p.id.clone(), p.value.clone())).collect()
}

pub fn workflow() -> DemoWorkflow {
    let w = &raw().workflow;
    let nodes = w
        .nodes
        .iter()
        .map(|n| Node {
            id: n.id.clone(),
            label: n.label.clone(),
            x: n.position[0],
            y: n.position[1],
            kind: n.kind,
            profile: n.profile.clone(),
            prompt: n.prompt.clone(),
            max_calls: n.max_calls,
            timeout_mins: 30,
            trigger: Default::default(),
        })
        .collect();
    let edges = w
        .edges
        .iter()
        .map(|e| {
            let tag = if e.kind == EdgeKind::Before { "-before" } else { "" };
            Edge { id: format!("{}-{}{tag}", e.from, e.to), from: e.from.clone(), to: e.to.clone(), kind: e.kind }
        })
        .collect();
    DemoWorkflow { id: w.id.clone(), name: w.name.clone(), workflow: Workflow { nodes, edges } }
}

/// First-start setup the user accepted: the demo project with its images (building them), profiles,
/// secrets, variables, injections and workflow. What exists already is left alone. Returns the project's id.
#[tauri::command]
pub fn provision_starters(app: tauri::AppHandle) -> Result<String, String> {
    let id = project_id();
    if crate::config::projects::load(&app, id).is_none() {
        crate::config::projects::store(&app, id, &project())?;
    }
    let scope = Scope::project(id);
    crate::vm::images::provision_defaults(&app, &scope, false)?;
    crate::config::profiles::provision_defaults(&app, id, false)?;
    crate::config::secrets::provision_demo(&app, &scope, false)?;
    crate::config::variables::provision_demo(&app, &scope, false)?;
    crate::config::injections::provision_demo(&app, &scope, false)?;
    crate::workflow::provision_defaults(&app, id)?;
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parts_agree_with_each_other() {
        let images: Vec<String> = images().into_iter().map(|(id, _)| id).collect();
        let profiles = profiles();
        for (id, p) in &profiles {
            assert!(p.image == crate::vm::images::BASE || images.contains(&p.image), "profile {id}: unknown image {}", p.image);
        }
        let w = workflow().workflow;
        assert!(w.validate().is_ok() && w.problems(&profiles.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()).is_empty());
        for n in w.nodes.iter().filter(|n| n.kind == NodeKind::Agent) {
            assert!(profiles.iter().any(|(id, _)| *id == n.profile), "node {}: unknown profile {}", n.id, n.profile);
        }
        for i in injections() {
            assert!(secrets().contains(&i.secret), "injection {}: unknown secret {}", i.name, i.secret);
        }
    }
}
