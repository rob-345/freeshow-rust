use freeshow_ffmpeg_download::*;
use serde_json::{json, Value};
use std::{
    io::{self, BufRead},
    time::{Duration, Instant},
};
#[tokio::main(flavor = "current_thread")]
async fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() < 1024 * 1024);
        let input: Value = serde_json::from_str(&line).unwrap();
        let result = run(input).await;
        println!(
            "{}",
            match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) =>
                    json!({"ok":false,"error":error,"completionPlan":source_completion(false,Some(&error.message))}),
            }
        );
    }
}
async fn run(input: Value) -> Result<Value> {
    match input["op"].as_str().unwrap_or("download") {
        "table" => Ok(json!(download_url(
            input["platform"].as_str().unwrap(),
            input["arch"].as_str().unwrap()
        ))),
        "progress" => Ok(json!(progress_for_bytes(
            input["received"].as_u64().unwrap(),
            input["total"].as_f64().unwrap()
        ))),
        "completion" => Ok(json!(source_completion(
            input["installed"].as_bool().unwrap(),
            input["error"].as_str()
        ))),
        _ => {
            let controlled_test = Some(ControlledTestConfig::loopback(
                input["url"]
                    .as_str()
                    .expect("fixture-only loopback URL required"),
            )?);
            let mut config = Config {
                controlled_test,
                ..Config::default()
            };
            if let Some(value) = input["compressedLimit"].as_u64() {
                config.compressed_bytes = value;
            }
            if let Some(value) = input["uncompressedLimit"].as_u64() {
                config.uncompressed_bytes = value;
            }
            if let Some(value) = input["headerLimit"].as_u64() {
                config.gzip_header_bytes = value as usize;
            }
            if let Some(value) = input["memberLimit"].as_u64() {
                config.gzip_members = value as usize;
            }
            if let Some(value) = input["redirectLimit"].as_u64() {
                config.redirects = value as usize;
            }
            if let Some(value) = input["timeoutMs"].as_u64() {
                config.timeout = Duration::from_millis(value);
            }
            let service = DownloadService::start(config)?;
            let owner = DownloadOwner::new("fixture", 1, input["root"].as_str().unwrap())?;
            let plan = plan_for_host(
                input["platform"].as_str().unwrap_or("linux"),
                input["arch"].as_str().unwrap_or("x64"),
                input["root"].as_str().unwrap(),
                owner.clone(),
            )?;
            let result=async{
            let ticket=service.begin(plan,CancellationToken::new(),Instant::now()+Duration::from_secs(10))?;let progress=ticket.progress();let prepared=ticket.finish().await?;let stats=prepared.stats.clone();let accepted=prepared.accept(&owner)?;
            // Test-only bounded inspection of inert fixture bytes. Production
            // worker never reads the complete artifact into memory or installs it.
            let view=accepted.verify_for_install(&owner)?;assert!(std::fs::metadata(view.staged_path).unwrap().len()<=1024*1024);
            let bytes=std::fs::read(view.staged_path).unwrap();let install=json!({"destination":view.destination,"mode":view.mode,"postInstall":view.post_install_commands(),"clearResolverCacheAfterPostInstall":true});
            let latest=progress.borrow().clone();accepted.finish().await?;
            Ok(json!({"bytes":bytes,"stats":stats,"install":install,"latestProgress":latest,"completionPlanIfInstalled":source_completion(true,None),"remainingEntries":std::fs::read_dir(input["root"].as_str().unwrap()).unwrap().count()}))
        }.await;
            let shutdown = service.shutdown().await;
            match (result, shutdown) {
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Ok(value), Ok(())) => Ok(value),
            }
        }
    }
}
