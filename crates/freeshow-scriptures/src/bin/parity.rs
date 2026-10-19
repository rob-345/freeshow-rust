use freeshow_scriptures::{CancellationToken, Limits, ScriptureContext, ScriptureService};
use serde_json::{json, Value};
use std::io::{self, BufRead};
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let service = ScriptureService::new(Limits::default()).unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() <= 1024 * 1024, "fixture request bound");
        let input: Value = serde_json::from_str(&line).unwrap();
        let result = async {
            let mut context = ScriptureContext::new(
                input["dataRoot"].as_str().unwrap(),
                input["documentsRoot"].as_str().unwrap(),
            )?;
            if input["afterDirectoryAttempt"] == true {
                context = context.after_directory_attempt();
            }
            service
                .load(&context, &input["payload"], CancellationToken::new())
                .await
        }
        .await;
        println!(
            "{}",
            match result {
                Ok(reply) => json!({"ok":true,"complete":reply.is_complete(),"reply":reply}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        );
    }
    service.shutdown().await.unwrap();
}
