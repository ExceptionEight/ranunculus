//! The note-taker: in the background, once the conversation has been quiet for IDLE s, the model in use turns its new
//! exchanges into notes about the user (memory facts), in batches of NOTE_BATCH; then the prompt of a model at home
//! is re-cached.
use crate::brain::Brain;
use crate::llm::Llm;
use crate::memory::{facts_messages, facts_schema, Exchange};
use crate::session::{log, Shared};
use crate::text::now_s;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// a model drops from a batch of 30 some of the facts it keeps from a short one
const NOTE_BATCH: usize = 12;
const IDLE: f64 = 180.0;

/// One batch: `todo` exchanges → note operations, applied. → (added, updated, removed)
pub async fn take_notes(brain: &Brain, llm: &Llm, todo: &[Exchange]) -> Result<(usize, usize, usize), String> {
    let mem = brain.memory.as_ref().ok_or("no memory")?;
    let body = json!({"messages": facts_messages(&mem.facts(), todo), "temperature": 0.2, "max_tokens": 1200,
                      "chat_template_kwargs": {"enable_thinking": false},
                      "response_format": {"type": "json_schema", "json_schema": {"name": "notes", "schema": facts_schema()}}});
    // a model at home: in its turn with the warm-up and the requests
    let r = brain.complete(llm, &body, Duration::from_secs(300)).await.map_err(|e| e.to_string())?;
    let content = r["choices"][0]["message"]["content"].as_str().ok_or("notes: no content")?;
    let ops: Value = serde_json::from_str(content).map_err(|e| format!("notes: {e}"))?;
    let n = mem.apply_facts(&ops, &format!("{}-{}", todo[0].id, todo[todo.len() - 1].id))?;
    mem.set_meta("facts_upto", &todo[todo.len() - 1].id.to_string())?;
    Ok(n)
}

pub async fn run(shared: Arc<Shared>) {
    let brain = shared.brain.clone();
    let Some(mem) = brain.memory.clone() else { return };
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        if shared.busy() || now_s() - brain.last() < IDLE {
            continue;
        }
        let upto: i64 = mem.meta("facts_upto").and_then(|s| s.parse().ok()).unwrap_or(0);
        let todo: Vec<Exchange> = match mem.since_id(upto) {
            Ok(v) => v.into_iter().take(NOTE_BATCH).collect(),
            Err(e) => {
                log("server", format!("notes: {e}"));
                tokio::time::sleep(Duration::from_secs(120)).await;
                continue;
            }
        };
        if todo.is_empty() {
            continue;
        }
        // no model yet: the exchanges wait
        let Some(llm) = shared.providers.llm() else { continue };
        let t = now_s();
        match take_notes(&brain, &llm, &todo).await {
            Ok(n) => {
                log(
                    "server",
                    format!(
                        "notes: +{} ~{} -{} from {} exchanges ({:.1} s), {} in all",
                        n.0,
                        n.1,
                        n.2,
                        todo.len(),
                        now_s() - t,
                        mem.facts().len()
                    ),
                );
                let last = todo[todo.len() - 1].id;
                if !shared.busy() && mem.since_id(last).map(|v| v.is_empty()).unwrap_or(false) {
                    // re-cache once, after the last batch
                    if let Some(s) = brain.prewarm(&llm).await {
                        log("server", format!("   (the prompt warmed up in {s:.1} s)"));
                    }
                }
            }
            Err(e) => {
                log("server", format!("notes: failed: {e}"));
                tokio::time::sleep(Duration::from_secs(120)).await;
            }
        }
    }
}
