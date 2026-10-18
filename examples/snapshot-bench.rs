//! Board-only replacement for the application during exclusive snapshot measurements.
//! See docs/snapshot-validation.md. No additional HTTP endpoint is installed.
use one_kvm::{
    runtime::RuntimeBuilder,
    video::snapshot::{SnapshotTarget, DEFAULT_TIMEOUT},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        cfg!(debug_assertions),
        "Use a debug build of this measurement tool"
    );
    anyhow::ensure!(
        std::path::Path::new("/dev/video_cap").exists(),
        "This tool runs only on the VIM4 board"
    );
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 5,
        "Usage: snapshot-bench DATA_DIR BIND_ADDRESS fit|region COUNT"
    );
    let count: usize = args[4].parse()?;
    anyhow::ensure!((1..=1000).contains(&count), "COUNT must be 1..1000");
    anyhow::ensure!(
        matches!(args[3].as_str(), "fit" | "region"),
        "Target must be fit or region"
    );
    let listener = tokio::net::TcpListener::bind(&args[2]).await?;
    let mut runtime = RuntimeBuilder::new(PathBuf::from(&args[1])).build().await?;
    let router = runtime.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    eprintln!(
        "PID {}. Configure codec/viewers/recording now; measurements start in 30 seconds.",
        std::process::id()
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    let mut durations = Vec::new();
    for index in 0..count {
        let state = runtime.state();
        let size = state.snapshot.source_size().await;
        let target = if args[3] == "fit" {
            SnapshotTarget::Fit {
                max_long_edge: 1280,
            }
        } else if let Some((w, h)) = size {
            SnapshotTarget::Region {
                x: w / 4,
                y: h / 4,
                w: w / 2,
                h: h / 2,
                max_long_edge: 1280,
                max_upscale: 4.0,
            }
        } else {
            SnapshotTarget::Fit {
                max_long_edge: 1280,
            }
        };
        let started = Instant::now();
        let result = state.snapshot.capture(target, 80, DEFAULT_TIMEOUT).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(frame) => {
                durations.push(elapsed_ms);
                println!(
                    "{}",
                    serde_json::json!({"index":index,"target":args[3],"elapsed_ms":elapsed_ms,
                    "bytes":frame.jpeg.len(),"width":frame.width,"height":frame.height,
                    "source_width":frame.source_width,"source_height":frame.source_height})
                );
            }
            Err(error) => println!(
                "{}",
                serde_json::json!({"index":index,"target":args[3],"elapsed_ms":elapsed_ms,"error":error.to_string()})
            ),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    durations.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        durations
            .get(((durations.len() as f64 * p).ceil() as usize).saturating_sub(1))
            .copied()
    };
    println!(
        "{}",
        serde_json::json!({"samples":count,"successful":durations.len(),"p50_ms":percentile(0.5),"p95_ms":percentile(0.95)})
    );
    server.abort();
    let _ = server.await;
    runtime.shutdown().await;
    Ok(())
}
