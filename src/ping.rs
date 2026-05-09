//! `ping` subcommand — bidi-stream RTT measurement against a tolki-server.
//!
//! HASH-8 (2026-05-08): protocol surface (PingFrame / PongFrame / method-id /
//! QuicTransport) lives in `tolki-client`'s `wire_client` module so this CLI
//! exercises the same codec the GUI uses. We keep only orchestration here:
//! tick loop, RTT bookkeeping, summary print.
//!
//! Wire flow (client side):
//!   1. STREAM-OPEN with empty init (NoPayload).
//!   2. STREAM-CLIENT-CHUNK (PingFrame) per tick — `client_timestamp_ms` set
//!      to the current Unix-ms wall clock at send time.
//!   3. STREAM-SERVER-CHUNK (PongFrame) inbound — server echoes
//!      `client_timestamp_ms` verbatim, so RTT = `now - pong.client_timestamp_ms`
//!      без локальной seq → send-time map.
//!   4. STREAM-CLIENT-END at deadline / Ctrl+C; await STREAM-SERVER-END (≤ 2 s).
//!
//! Cannot use `RpcClient` here: its reader_loop drops streaming frames on the
//! floor and `transport.recv_stream()` is single-call. We feed inbound frames
//! into `StreamRegistry` ourselves through a dedicated dispatcher task.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::{Multiaddr, PeerId};
use tokio::signal;
use tolki_client::wire_client::quic_transport::QuicTransport;
use tolki_client::wire_client::{
    NoPayload, PingFrame, PongFrame, PING_PONG_METHOD_ID, PING_PONG_UNARY_METHOD_ID,
};
use tolki_wire::rpc::stream::{decode_stream_frame, open_bidi_stream, StreamRegistry};
use tolki_wire::rpc::{RpcClient, Transport};
use tracing::{debug, warn};

/// How long to wait for STREAM-SERVER-END after sending STREAM-CLIENT-END.
const SERVER_END_WAIT: Duration = Duration::from_secs(2);

/// Running stats for a ping session.
#[derive(Debug, Default)]
pub struct PingStats {
    pings_sent: u64,
    pongs_received: u64,
    rtt_samples_ms: Vec<u64>,
    rtt_min_ms: u64,
    rtt_max_ms: u64,
}

impl PingStats {
    pub fn new() -> Self {
        Self {
            rtt_min_ms: u64::MAX,
            ..Default::default()
        }
    }

    fn record_rtt(&mut self, rtt_ms: u64) {
        self.pongs_received += 1;
        self.rtt_samples_ms.push(rtt_ms);
        if rtt_ms < self.rtt_min_ms {
            self.rtt_min_ms = rtt_ms;
        }
        if rtt_ms > self.rtt_max_ms {
            self.rtt_max_ms = rtt_ms;
        }
    }

    fn loss_pct(&self) -> f64 {
        if self.pings_sent == 0 {
            return 0.0;
        }
        let lost = self.pings_sent.saturating_sub(self.pongs_received);
        (lost as f64) * 100.0 / (self.pings_sent as f64)
    }

    fn rtt_median_ms(&self) -> u64 {
        if self.rtt_samples_ms.is_empty() {
            return 0;
        }
        let mut sorted = self.rtt_samples_ms.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }
}

/// Drive the `ping` subcommand: connect, open bidi stream, send pings at
/// `interval_ms`, print RTTs, terminate at `duration_s` or Ctrl+C, print summary.
pub async fn run_ping(
    peer_id: PeerId,
    multiaddr: Multiaddr,
    interval_ms: u64,
    duration_s: u64,
) -> Result<()> {
    if interval_ms == 0 {
        anyhow::bail!("--interval-ms must be > 0");
    }
    if duration_s == 0 {
        anyhow::bail!("--duration-s must be > 0");
    }

    let transport = QuicTransport::connect(peer_id, vec![multiaddr])
        .await
        .context("QUIC connect failed")?;
    let transport: Arc<dyn Transport> = Arc::new(transport);
    let registry = Arc::new(StreamRegistry::new());

    // Dispatcher task — drains transport.recv_stream() and forwards every
    // streaming frame to the registry. Spawn before open_bidi_stream so the
    // STREAM-OPEN ack / server frames have a consumer ready.
    let dispatcher = tokio::spawn(dispatcher_loop(
        Arc::clone(&transport),
        Arc::clone(&registry),
    ));

    let mut handle = open_bidi_stream::<NoPayload, PingFrame, PongFrame, NoPayload>(
        Arc::clone(&transport),
        Arc::clone(&registry),
        *PING_PONG_METHOD_ID,
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("open bidi stream failed: {e}"))?;

    println!("✓ ping stream open  stream_id = {}", handle.stream_id());
    println!(
        "  sending every {} ms for {} s — Ctrl+C к stop early",
        interval_ms, duration_s
    );

    let interval = Duration::from_millis(interval_ms);
    let deadline = Instant::now() + Duration::from_secs(duration_s);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;

    let mut stats = PingStats::new();
    let mut next_seq: u64 = 0;

    loop {
        tokio::select! {
            biased;

            _ = signal::ctrl_c() => {
                println!("\n  ctrl-c received — closing send side");
                if let Err(err) = handle.close_send().await {
                    warn!(error = %err, "close_send failed");
                }
                break;
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                debug!("deadline reached, closing send side");
                if let Err(err) = handle.close_send().await {
                    warn!(error = %err, "close_send failed");
                }
                break;
            }

            _ = tick.tick() => {
                next_seq += 1;
                let seq = next_seq;
                let client_timestamp_ms = current_unix_ms();

                if let Err(err) = handle.send_chunk(PingFrame { seq, client_timestamp_ms }).await {
                    warn!(seq, error = %err, "send_chunk failed, terminating");
                    break;
                }
                stats.pings_sent += 1;
            }

            recv = handle.recv() => {
                match recv {
                    Some(Ok(pong)) => {
                        record_pong(&pong, &mut stats);
                    }
                    Some(Err(err)) => {
                        warn!(error = %err, "recv error, terminating");
                        break;
                    }
                    None => {
                        debug!("server closed stream cleanly");
                        break;
                    }
                }
            }
        }
    }

    // After STREAM-CLIENT-END, drain any in-flight pongs (or STREAM-SERVER-END).
    drain_pending_pongs(&mut handle, &mut stats).await;

    print_summary(&stats);

    drop(handle);
    dispatcher.abort();
    let _ = dispatcher.await;

    Ok(())
}

/// Drive the unary-mode `ping` flow: connect, then per-tick fire an
/// independent UNARY-REQUEST carrying [`PingFrame`], await the matching
/// [`PongFrame`] response. Used while the server's `register_bidi` adapter
/// is still pending — server's Phase 1 ships `register_unary` only.
///
/// Each tick = one full unary RPC (FRAME_UNARY_REQUEST → FRAME_UNARY_RESPONSE).
/// RTT is computed from the echoed `client_timestamp_ms` (server returns it
/// verbatim per canonical WIT spec) so no local seq → send-time map is needed.
pub async fn run_ping_unary(
    peer_id: PeerId,
    multiaddr: Multiaddr,
    interval_ms: u64,
    duration_s: u64,
) -> Result<()> {
    if interval_ms == 0 {
        anyhow::bail!("--interval-ms must be > 0");
    }
    if duration_s == 0 {
        anyhow::bail!("--duration-s must be > 0");
    }

    let transport = QuicTransport::connect(peer_id, vec![multiaddr])
        .await
        .context("QUIC connect failed")?;
    let client = RpcClient::new(Arc::new(transport.clone()));

    println!(
        "✓ unary ping mode  (interval={} ms, duration={} s)",
        interval_ms, duration_s
    );

    let stats = run_unary_tick_loop(&client, interval_ms, duration_s).await;
    print_summary(&stats);
    transport.close();
    Ok(())
}

/// Tick loop for unary-mode ping. Returns the accumulated [`PingStats`] when
/// the deadline elapses or Ctrl+C fires. Extracted из [`run_ping_unary`] so
/// каждая half (setup / orchestration) stays under 30 lines per Pavel
/// directive on small functions.
async fn run_unary_tick_loop(
    client: &RpcClient<QuicTransport>,
    interval_ms: u64,
    duration_s: u64,
) -> PingStats {
    let interval = Duration::from_millis(interval_ms);
    let deadline = Instant::now() + Duration::from_secs(duration_s);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;

    let mut stats = PingStats::new();
    let mut next_seq: u64 = 0;

    loop {
        tokio::select! {
            biased;

            _ = signal::ctrl_c() => {
                println!("\n  ctrl-c received — stopping");
                break;
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                debug!("deadline reached");
                break;
            }
            _ = tick.tick() => {
                send_unary_ping(client, &mut next_seq, &mut stats).await;
            }
        }
    }
    stats
}

/// Send а single unary ping и handle the pong (или error). `next_seq` is
/// incremented before send; [`PingStats::pings_sent`] is bumped only after
/// the request leaves the wire so failures don't inflate the loss numerator
/// unfairly при connect failure mid-loop.
async fn send_unary_ping(
    client: &RpcClient<QuicTransport>,
    next_seq: &mut u64,
    stats: &mut PingStats,
) {
    *next_seq += 1;
    let seq = *next_seq;
    let client_timestamp_ms = current_unix_ms();
    let req = PingFrame {
        seq,
        client_timestamp_ms,
    };
    stats.pings_sent += 1;

    match client
        .call::<PingFrame, PongFrame>(*PING_PONG_UNARY_METHOD_ID, req)
        .await
    {
        Ok(pong) => record_pong(&pong, stats),
        Err(err) => {
            eprintln!("[seq={}] error: {}", seq, err);
        }
    }
}

/// Dispatcher loop: drains `transport.recv_stream()`, decodes each payload as
/// a streaming frame, and forwards it to the registry.
async fn dispatcher_loop(transport: Arc<dyn Transport>, registry: Arc<StreamRegistry>) {
    let mut stream = transport.recv_stream();
    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(b) => b,
            Err(err) => {
                debug!(error = %err, "dispatcher: inbound error, exiting");
                return;
            }
        };
        match decode_stream_frame(&bytes) {
            Ok(frame) => {
                if let Err(err) = registry.dispatch(frame).await {
                    debug!(error = %err, "dispatcher: registry dispatch failed");
                }
            }
            Err(err) => {
                debug!(error = %err, "dispatcher: malformed frame dropped");
            }
        }
    }
    debug!("dispatcher: transport recv_stream ended, exiting");
}

/// Compute RTT from the echoed `client_timestamp_ms` and record the sample.
///
/// Per canonical WIT spec the server echoes the originating ping's
/// `client_timestamp_ms` verbatim, so RTT is simply `now - echo` —
/// no seq → send-time bookkeeping required on the client side.
fn record_pong(pong: &PongFrame, stats: &mut PingStats) {
    let now_ms = current_unix_ms();
    let rtt_ms = now_ms.saturating_sub(pong.client_timestamp_ms).max(0) as u64;
    println!(
        "[seq={}] RTT={} ms  server_ts={}",
        pong.seq, rtt_ms, pong.server_timestamp_ms
    );
    stats.record_rtt(rtt_ms);
}

/// After close_send, wait up to [`SERVER_END_WAIT`] for late pongs and the
/// server's STREAM-SERVER-END frame.
async fn drain_pending_pongs(
    handle: &mut tolki_wire::rpc::stream::BidiStreamHandle<PingFrame, PongFrame, NoPayload>,
    stats: &mut PingStats,
) {
    let drain_deadline = Instant::now() + SERVER_END_WAIT;
    loop {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let recv = tokio::time::timeout(remaining, handle.recv()).await;
        match recv {
            Ok(Some(Ok(pong))) => record_pong(&pong, stats),
            Ok(Some(Err(err))) => {
                warn!(error = %err, "drain: recv error");
                break;
            }
            Ok(None) => {
                debug!("drain: server closed stream");
                break;
            }
            Err(_) => {
                debug!("drain: server-end timeout");
                break;
            }
        }
    }
}

fn print_summary(stats: &PingStats) {
    println!();
    println!("=== ping summary ===");
    println!("sent:     {}", stats.pings_sent);
    println!("received: {}", stats.pongs_received);
    println!("loss:     {:.1}%", stats.loss_pct());
    if stats.pongs_received > 0 {
        println!("rtt min:  {} ms", stats.rtt_min_ms);
        println!("rtt max:  {} ms", stats.rtt_max_ms);
        println!("rtt med:  {} ms", stats.rtt_median_ms());
    } else {
        println!("rtt min:  -");
        println!("rtt max:  -");
        println!("rtt med:  -");
    }
}

/// Current Unix time в milliseconds.
fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(i64::MAX as u64)) as i64)
        .unwrap_or(0)
}
