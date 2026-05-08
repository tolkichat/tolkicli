//! `tolkicli` — Tolki terminal CLI sharing the GUI's wire-protocol stack.
//!
//! Pavel directive 2026-05-08: «Толки CLI он должен использовать тот же код.
//! И те же библиотеки. Что использует и наша программа с GUI.» — иначе
//! testing through tolkicli proves nothing about the GUI behaviour.
//!
//! Therefore this crate depends on `tolki-client` с feature `cli` (no asr /
//! no llm) which is ort-free. All wire / transport / codec logic comes from
//! the shared lib; this crate owns only the CLI argparse + ping orchestration.

mod ping;
mod register;

use std::str::FromStr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libp2p::{Multiaddr, PeerId};
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "tolkicli",
    about = "Standalone Tolki ping client (no ort dependency)"
)]
struct Args {
    /// Server's libp2p peer-id (z-base-32 / base58btc string).
    #[arg(long)]
    server_peer_id: String,

    /// Server multiaddr (e.g. `/ip4/127.0.0.1/udp/4434/quic-v1`).
    #[arg(long)]
    server_multiaddr: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Open `tolki:ping@1.0.0/ping/ping-pong` bidi stream — sends pings at
    /// `interval_ms`, prints RTT for each pong, exits after `duration_s` or
    /// Ctrl+C. Prints summary on exit.
    Ping {
        /// Send interval in milliseconds (default 1000 ms = 1 Hz).
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,

        /// Total run duration in seconds (default 30 s).
        #[arg(long, default_value_t = 30)]
        duration_s: u64,
    },

    /// Register а new identity via BIP-39 mnemonic. Generates fresh 24-word
    /// phrase by default; pass `--mnemonic "<phrase>"` to register an
    /// existing identity. Device-id persisted к `~/.tolki/device-id.bin`.
    Register {
        /// Existing BIP-39 phrase (12 or 24 words). Omit to generate fresh.
        #[arg(long)]
        mnemonic: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let args = Args::parse();
    let peer_id = PeerId::from_str(&args.server_peer_id)
        .with_context(|| format!("invalid --server-peer-id: {:?}", args.server_peer_id))?;
    let multiaddr = Multiaddr::from_str(&args.server_multiaddr)
        .with_context(|| format!("invalid --server-multiaddr: {:?}", args.server_multiaddr))?;
    info!(%peer_id, %multiaddr, "tolki-cli — connecting");

    match args.command {
        Command::Ping {
            interval_ms,
            duration_s,
        } => ping::run_ping(peer_id, multiaddr, interval_ms, duration_s).await,
        Command::Register { mnemonic } => {
            register::run_register(peer_id, multiaddr, mnemonic).await
        }
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}
