use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use log::info;

use akikazu_remote::cli::{Cli, Command, ExchangeArgs};
use akikazu_remote::exchange::{
    decode_description, encode_description, prompt_for_token, wait_for_token_file, write_token,
};
use akikazu_remote::network::diagnostics::CandidateSummary;
use akikazu_remote::network::peer::Peer;
use akikazu_remote::protocol::Role;
use akikazu_remote::session::{SessionReport, run as run_session};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Offer(args) => {
            let report_path = args.report.clone();
            let report = run_offer(args).await?;
            finish(report, &report_path)
        }
        Command::Answer(args) => {
            let report_path = args.report.clone();
            let report = run_answer(args).await?;
            finish(report, &report_path)
        }
    }
}

fn finish(report: SessionReport, report_path: &Path) -> Result<ExitCode> {
    report.print_summary();
    report.write_json(report_path)?;
    println!("Diagnostics: {}", report_path.display());
    Ok(if report.result.is_success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn run_offer(args: ExchangeArgs) -> Result<SessionReport> {
    let mut peer = Peer::new(args.network_config()).await?;
    let data_channel = peer.create_data_channel().await?;

    let offer = peer.pc.create_offer(None).await.context("create offer")?;
    peer.pc
        .set_local_description(offer)
        .await
        .context("set local offer")?;
    peer.wait_for_gathering().await?;

    let local = peer
        .pc
        .local_description()
        .await
        .ok_or_else(|| anyhow!("no local description after ICE gathering"))?;
    let token = encode_description(&local)?;
    emit_token(args.offer_file.as_deref(), &token, "offer")?;
    print_candidates(&peer.candidate_summary());

    let answer_token = match args.answer_file.as_deref() {
        Some(path) => wait_for_token_file(path, args.exchange_timeout()).await?,
        None => {
            eprintln!("Send the offer token to the other PC, then paste the answer token below.");
            prompt_for_token("Answer token:").await?
        }
    };
    let answer = decode_description(&answer_token)?;
    peer.pc
        .set_remote_description(answer)
        .await
        .context("set remote answer")?;

    println!("P2P status: negotiating...");
    let report = run_session(peer, data_channel, Role::Offerer, args.session_config()).await?;
    info!("offer session finished: {:?}", report.result);
    Ok(report)
}

async fn run_answer(args: ExchangeArgs) -> Result<SessionReport> {
    let offer_token = match args.offer_file.as_deref() {
        Some(path) => wait_for_token_file(path, args.exchange_timeout()).await?,
        None => {
            eprintln!("Paste the offer token from the other PC below.");
            prompt_for_token("Offer token:").await?
        }
    };
    let offer = decode_description(&offer_token)?;

    let mut peer = Peer::new(args.network_config()).await?;
    peer.pc
        .set_remote_description(offer)
        .await
        .context("set remote offer")?;
    let answer = peer.pc.create_answer(None).await.context("create answer")?;
    peer.pc
        .set_local_description(answer)
        .await
        .context("set local answer")?;
    peer.wait_for_gathering().await?;

    let local = peer
        .pc
        .local_description()
        .await
        .ok_or_else(|| anyhow!("no local description after ICE gathering"))?;
    let token = encode_description(&local)?;
    emit_token(args.answer_file.as_deref(), &token, "answer")?;
    print_candidates(&peer.candidate_summary());

    println!("P2P status: waiting for peer...");
    let data_channel = peer
        .wait_for_channel(args.session_config().connect_timeout)
        .await?;
    let report = run_session(peer, data_channel, Role::Answerer, args.session_config()).await?;
    info!("answer session finished: {:?}", report.result);
    Ok(report)
}

fn emit_token(path: Option<&Path>, token: &str, label: &str) -> Result<()> {
    match path {
        Some(path) => {
            write_token(path, token)?;
            eprintln!("{label} token written to {}", path.display());
        }
        None => println!("{token}"),
    }
    Ok(())
}

fn print_candidates(summary: &CandidateSummary) {
    eprintln!(
        "Local ICE candidates: {} total (IPv4 host {}, IPv6 host {}, IPv4 srflx {}, IPv6 srflx {})",
        summary.total, summary.host_ipv4, summary.host_ipv6, summary.srflx_ipv4, summary.srflx_ipv6
    );
    eprintln!("STUN: {}", summary.stun_status());
}

fn init_logging(verbose: u8) {
    let default_filter = if verbose > 0 {
        "akikazu_remote=debug,webrtc=debug,rtc=debug"
    } else {
        "akikazu_remote=info,webrtc=error,rtc=off"
    };
    let _ =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter))
            .format_timestamp_millis()
            .try_init();
}
