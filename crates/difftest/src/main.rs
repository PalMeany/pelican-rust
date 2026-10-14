//! Differential test runner: original Pelican panel vs pelican-rs.
//!
//!   scripts/difftest --backend mariadb [-- --filter <substr>] [--self-check] [--show]
//!
//! For every scenario: reset DB + load fixtures, run against the original
//! panel, render a normalized report; repeat for the second target (pelican-rs,
//! or the original again with --self-check) and compare the reports.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use difftest::db::{TestDb, config_for};
use difftest::mock_wings::MockWings;
use difftest::runner::{self, Ctx};
use difftest::scenario::load_scenarios;
use laravel_compat::Encrypter;

/// APP_KEY shared by both panels (difftest/compose.yml).
const APP_KEY: &str = "base64:Rm9yRGlmZmVyZW50aWFsVGVzdGluZ09ubHkhIDMyQiE=";
const ORIG_URL: &str = "http://panel-orig";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "mariadb")]
    backend: String,
    /// Only scenarios whose name contains this substring.
    #[arg(long)]
    filter: Option<String>,
    /// Compare the original panel against itself (validates normalization).
    #[arg(long)]
    self_check: bool,
    /// Print the normalized reports even when they match.
    #[arg(long)]
    show: bool,
    /// Base URL of an already running pelican-rs (default: start in-process).
    #[arg(long)]
    rust_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../difftest");
    let enc = Encrypter::from_env_values(APP_KEY, None)?;
    let db = TestDb::connect(&config_for(&args.backend)?)
        .await
        .context("connecting to difftest DB")?;
    let wings = MockWings::start()
        .await
        .context("starting mock wings on :8080")?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()?;
    let ctx = Ctx {
        db: &db,
        wings: &wings,
        enc: &enc,
        http,
        fixtures_dir: root.join("fixtures"),
    };

    let second_url = if args.self_check {
        ORIG_URL.to_string()
    } else if let Some(u) = &args.rust_url {
        u.clone()
    } else {
        difftest::rust_target::start(&args.backend, APP_KEY).await?
    };

    let scenarios = load_scenarios(&root.join("scenarios"), args.filter.as_deref())?;
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    for s in &scenarios {
        if let Some(reason) = &s.skip {
            println!("SKIP {} — {reason}", s.name);
            skip += 1;
            continue;
        }
        if s.backends
            .as_ref()
            .is_some_and(|b| !b.iter().any(|x| x == &args.backend))
        {
            continue;
        }
        let rows_text = serde_yaml::to_string(&runner::merged_rows(&ctx, s)?)?;
        let a = runner::run(&ctx, s, ORIG_URL)
            .await
            .with_context(|| format!("{}: original", s.name));
        let b = runner::run(&ctx, s, &second_url)
            .await
            .with_context(|| format!("{}: second", s.name));
        let (a, b) = match (a, b) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => {
                println!("FAIL {} [{}]", s.name, args.backend);
                if let Err(e) = a {
                    println!("  original: {e:#}");
                }
                if let Err(e) = b {
                    println!("  second: {e:#}");
                }
                fail += 1;
                continue;
            }
        };
        let ra = runner::report(&enc, s, &rows_text, &a)?;
        let rb = runner::report(&enc, s, &rows_text, &b)?;
        if ra == rb {
            println!("PASS {} [{}]", s.name, args.backend);
            if args.show {
                println!("{ra}");
            }
            pass += 1;
        } else {
            println!("FAIL {} [{}]  ({})", s.name, args.backend, s.file.display());
            let diff = similar::TextDiff::from_lines(&ra, &rb);
            print!(
                "{}",
                diff.unified_diff().context_radius(3).header(
                    "original",
                    if args.self_check {
                        "original#2"
                    } else {
                        "pelican-rs"
                    }
                )
            );
            fail += 1;
        }
    }
    println!(
        "\n{pass} passed, {fail} failed, {skip} skipped [{}]",
        args.backend
    );
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
