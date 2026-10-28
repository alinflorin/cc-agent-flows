//! What a coordinator answers and how the engine checks it against the chart.

use super::*;

/// What a coordinator answers: where `calls` go, or `finish` with a `result`, or `ask`.
pub const COORDINATOR_SCHEMA: &str = r#"{"type":"object","properties":{"action":{"type":"string","enum":["delegate","finish","ask"]},"calls":{"type":"array","items":{"type":"object","properties":{"to":{"type":"string"},"instructions":{"type":"string"}},"required":["to","instructions"]}},"result":{"type":"string"},"success":{"type":"boolean"},"questions":{"type":"array","items":{"type":"string"}},"notes":{"type":"string"},"assumptions":{"type":"array","items":{"type":"string"}}},"required":["action","notes"]}"#;

#[derive(Debug, PartialEq)]
pub enum Action {
    Delegate(Vec<(Opt, String)>),
    Finish { result: String, success: bool },
    Ask(Vec<String>),
}

#[derive(Debug, PartialEq)]
pub struct Decision {
    pub action: Action,
    pub notes: String,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

pub fn assumptions_of(output: Option<&Value>) -> Vec<String> {
    strings(output.and_then(|o| o.get("assumptions")))
}

/// Reads a coordinator's answer and checks it against the agents it may call.
pub fn parse_decision(output: Option<&Value>, options: &[Opt], finished: &std::collections::HashSet<String>) -> Result<Decision, String> {
    let Some(o) = output.and_then(Value::as_object) else {
        return Err("the answer was not the structured object with `action` and `notes`".into());
    };
    let notes = o.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
    let action = match o.get("action").and_then(Value::as_str).unwrap_or("") {
        "delegate" => {
            let calls = o.get("calls").and_then(Value::as_array).cloned().unwrap_or_default();
            if calls.is_empty() || calls.len() > MAX_PARALLEL_CALLS {
                return Err(format!("`calls` needs between 1 and {MAX_PARALLEL_CALLS} entries"));
            }
            let mut out = vec![];
            for c in &calls {
                let to = c.get("to").and_then(Value::as_str).unwrap_or("").trim();
                let Some(opt) = options.iter().find(|x| x.id == to).or_else(|| options.iter().find(|x| x.label.eq_ignore_ascii_case(to))) else {
                    let ids: Vec<&str> = options.iter().map(|x| x.id.as_str()).collect();
                    return Err(format!("`to` was {to:?}, which you may not call; the options are: {}", ids.join(", ")));
                };
                if let Some(missing) = opt.needs.iter().find(|a| !finished.contains(*a)) {
                    let name = options.iter().find(|x| &x.id == missing).map_or(missing.as_str(), |x| x.label.as_str());
                    return Err(format!("`{}` cannot be called yet: {name} must have finished first (in an earlier answer of yours)", opt.id));
                }
                let instructions = c.get("instructions").and_then(Value::as_str).unwrap_or("").trim().to_string();
                if instructions.is_empty() {
                    return Err(format!("the call to {} has no `instructions`", opt.id));
                }
                out.push((opt.clone(), instructions));
            }
            Action::Delegate(out)
        }
        "finish" => {
            let result = o.get("result").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if result.is_empty() {
                return Err("`finish` needs a `result`".into());
            }
            Action::Finish { result, success: o.get("success").and_then(Value::as_bool).unwrap_or(true) }
        }
        "ask" => {
            let questions = strings(o.get("questions"));
            if questions.is_empty() {
                return Err("`ask` needs `questions`".into());
            }
            Action::Ask(questions)
        }
        other => return Err(format!("`action` was {other:?}; it must be delegate, finish or ask")),
    };
    Ok(Decision { action, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::standard_workflow;

    #[test]
    fn decisions_are_checked_against_the_options() {
        let wf = standard_workflow();
        let opts = options(&wf, "pm");
        assert_eq!(opts.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["ba", "arch", "dev", "rev", "qa"]);
        let ok = parse_decision(Some(&json!({"action": "delegate", "calls": [{"to": "Developer", "instructions": "x"}], "notes": "n"})), &opts, &["arch".to_string()].into()).unwrap();
        assert!(matches!(&ok.action, Action::Delegate(c) if c[0].0.id == "dev"));
        // the developer comes after the architect
        let early = json!({"action": "delegate", "calls": [{"to": "dev", "instructions": "x"}], "notes": ""});
        assert!(parse_decision(Some(&early), &opts, &Default::default()).unwrap_err().contains("Architect"));
        for bad in [
            json!({"action": "delegate", "calls": [], "notes": ""}),
            json!({"action": "delegate", "calls": [{"to": "pm", "instructions": "x"}], "notes": ""}),
            json!({"action": "delegate", "calls": [{"to": "dev", "instructions": " "}], "notes": ""}),
            json!({"action": "finish", "notes": ""}),
            json!({"action": "ask", "questions": [], "notes": ""}),
            json!({"action": "dance", "notes": ""}),
        ] {
            assert!(parse_decision(Some(&bad), &opts, &Default::default()).is_err(), "{bad}");
        }
        let six: Vec<Value> = (0..6).map(|_| json!({"to": "dev", "instructions": "x"})).collect();
        assert!(parse_decision(Some(&json!({"action": "delegate", "calls": six, "notes": ""})), &opts, &Default::default()).is_err());
        assert!(parse_decision(None, &opts, &Default::default()).is_err());
        serde_json::from_str::<Value>(COORDINATOR_SCHEMA).unwrap();
    }
}
