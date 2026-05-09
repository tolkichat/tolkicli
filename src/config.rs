//! Persistent CLI configuration (`~/.config/tolki/config.toml`).
//!
//! CLI-1 phase 4 (2026-05-09): server flags на каждом subcommand'е делают
//! invocations verbose (`--server-peer-id …` + `--server-multiaddr …` каждый
//! раз). Этот модуль материализует bundled-defaults на первый запуск,
//! читает их при последующих, и лет user override per-call через explicit
//! flags. Same atomic-write discipline as `register.rs::write_identity_file`
//! (tmp + rename, parent dir mode 0700 on Unix).
//!
//! Schema layout:
//!
//! ```toml
//! schema_version = 1
//!
//! [server]
//! peer_id = "12D3KooW…"
//! multiaddr = "/ip4/…/udp/4434/quic-v1"
//! ```
//!
//! Future fields (log_level, default_format, default_profile…) plug in as
//! `Option<…>` / `#[serde(default)]` без bumping schema_version так long as
//! older builds can ignore them safely.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Schema version of the on-disk `config.toml`. Bump when the layout changes
/// in a backwards-incompatible way; older builds will refuse to load and tell
/// the user к migrate / regenerate.
const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Bundled default server peer-id — the live tolki-server-api QUIC endpoint
/// at the time of writing. New users get this on first run so `register` /
/// `ping` work с zero flags out of the box.
const DEFAULT_SERVER_PEER_ID: &str = "12D3KooWKvo4P6NAdhFDPrkU9RhZeuiVz5PkKj5WsRMfPrDcJknU";

/// Bundled default server multiaddr — same endpoint as
/// [`DEFAULT_SERVER_PEER_ID`] above.
const DEFAULT_SERVER_MULTIADDR: &str = "/ip4/139.99.100.210/udp/4434/quic-v1";

/// Top-level structure persisted к `~/.config/tolki/config.toml`. The leading
/// `schema_version` makes the format self-describing for forward compat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Layout version — 1 currently. Mismatch ⇒ load fails loudly.
    pub schema_version: u32,
    /// Server endpoint (peer-id + multiaddr).
    pub server: ServerConfig,
}

/// Server endpoint config — peer-id и multiaddr, both validated as
/// libp2p types at load time (string-typed for ergonomic TOML, but
/// `from_str`-checked).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// libp2p peer-id (z-base-32 / base58btc string).
    pub peer_id: String,
    /// libp2p multiaddr (e.g. `/ip4/.../udp/4434/quic-v1`).
    pub multiaddr: String,
}

/// Bundled defaults — the live tolki-server-api endpoint at ship time.
pub fn default_config() -> Config {
    Config {
        schema_version: CONFIG_SCHEMA_VERSION,
        server: ServerConfig {
            peer_id: DEFAULT_SERVER_PEER_ID.to_string(),
            multiaddr: DEFAULT_SERVER_MULTIADDR.to_string(),
        },
    }
}

/// Resolve the canonical config path: `${HOME}/.config/tolki/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir()
        .context("could not determine $HOME — set HOME env var")?;
    Ok(home.join(".config").join("tolki").join("config.toml"))
}

/// Read `~/.config/tolki/config.toml` if it exists, else write а fresh
/// default-config and return it. The bootstrap branch logs а one-line note
/// to **stderr** so stdout-piping consumers stay clean.
pub fn load_or_bootstrap() -> Result<Config> {
    let path = config_path()?;
    if path.exists() {
        return load_existing(&path);
    }
    let cfg = default_config();
    save(&cfg)?;
    eprintln!(
        "note: bootstrapped default config at {}",
        path.display()
    );
    Ok(cfg)
}

/// Parse an existing `config.toml`, validating schema-version и that
/// peer-id / multiaddr are both well-formed libp2p strings. Refuses to load
/// stale schema rather than silently overwriting hand-edits.
fn load_existing(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let cfg: Config = toml::from_str(&text)
        .with_context(|| format!("failed to parse {} as TOML", path.display()))?;
    if cfg.schema_version != CONFIG_SCHEMA_VERSION {
        bail!(
            "{} has schema_version {} but this build expects {} — migrate \
             or `tolkicli config reset --yes` to regenerate",
            path.display(),
            cfg.schema_version,
            CONFIG_SCHEMA_VERSION,
        );
    }
    validate_server_endpoint(&cfg.server, path)?;
    Ok(cfg)
}

/// Re-parse peer-id / multiaddr to surface а clear error if а hand-edit
/// produced а malformed string. Centralised so `load_existing` and `set`
/// share the exact same validation logic.
fn validate_server_endpoint(server: &ServerConfig, path: &Path) -> Result<()> {
    PeerId::from_str(&server.peer_id).with_context(|| {
        format!(
            "{} has invalid server.peer_id {:?}",
            path.display(),
            server.peer_id
        )
    })?;
    Multiaddr::from_str(&server.multiaddr).with_context(|| {
        format!(
            "{} has invalid server.multiaddr {:?}",
            path.display(),
            server.multiaddr
        )
    })?;
    Ok(())
}

/// Atomically write `cfg` к the canonical path: tmp + rename, parent dir
/// created с mode 0700 on Unix. Same discipline as
/// `register.rs::write_identity_file`.
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path()?;
    let parent = path
        .parent()
        .context("config path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    set_dir_mode_0700(parent)?;

    let body = toml::to_string_pretty(cfg)
        .context("failed to serialize config к TOML")?;
    let banner = "# tolki CLI config — managed by `tolkicli config`. Edit с care.\n";
    let contents = format!("{banner}{body}");

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, contents.as_bytes())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| {
        format!("failed to rename {} → {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// On Unix, tighten directory permissions to 0700. No-op on non-Unix.
#[cfg(unix)]
fn set_dir_mode_0700(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(dir)
        .with_context(|| format!("failed to stat {}", dir.display()))?
        .permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dir, perms)
        .with_context(|| format!("failed to chmod 0700 on {}", dir.display()))
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Print current config in а human-readable, two-column layout matching the
/// style of `tolkicli identity show`.
pub fn run_show() -> Result<()> {
    let path = config_path()?;
    let cfg = load_or_bootstrap()?;
    println!("config ({})", path.display());
    println!("  schema_version    {}", cfg.schema_version);
    println!("  server.peer-id    {}", cfg.server.peer_id);
    println!("  server.multiaddr  {}", cfg.server.multiaddr);
    Ok(())
}

/// Update а single setting и persist. Validates the new value before
/// touching disk — а malformed peer-id / multiaddr is rejected up-front.
pub fn run_set(key: &str, value: &str) -> Result<()> {
    let mut cfg = load_or_bootstrap()?;
    apply_set(&mut cfg, key, value)?;
    save(&cfg)?;
    println!("✓ updated {}", key);
    Ok(())
}

/// Mutate `cfg` for а supported `key`. Centralised here so future keys
/// (log_level, default_format…) get а single match arm к extend.
fn apply_set(cfg: &mut Config, key: &str, value: &str) -> Result<()> {
    match key {
        "server.peer-id" | "server.peer_id" => {
            PeerId::from_str(value)
                .with_context(|| format!("invalid server.peer-id: {:?}", value))?;
            cfg.server.peer_id = value.to_string();
            Ok(())
        }
        "server.multiaddr" => {
            Multiaddr::from_str(value)
                .with_context(|| format!("invalid server.multiaddr: {:?}", value))?;
            cfg.server.multiaddr = value.to_string();
            Ok(())
        }
        other => bail!(
            "unknown config key {:?} — supported keys: server.peer-id, server.multiaddr",
            other
        ),
    }
}

/// Reset to bundled defaults. With `yes = false` prompts on stdin and
/// aborts unless the user types `y` / `yes`.
pub fn run_reset(yes: bool) -> Result<()> {
    let path = config_path()?;
    if !yes && !confirm_reset(&path)? {
        println!("aborted — config left unchanged");
        return Ok(());
    }
    let cfg = default_config();
    save(&cfg)?;
    if !yes {
        println!("✓ reset {}", path.display());
    }
    info!(path = %path.display(), "config: reset to bundled defaults");
    Ok(())
}

/// Interactive y/N confirmation для `config reset`. Same shape as
/// `identity::confirm_wipe` for visual consistency.
fn confirm_reset(path: &Path) -> Result<bool> {
    println!(
        "about to overwrite {} with bundled defaults.",
        path.display()
    );
    print!("proceed? [y/N] ");
    io::stdout().flush().context("failed to flush stdout")?;

    let stdin = io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("failed to read confirmation from stdin")?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}
