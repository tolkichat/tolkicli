//! `register` subcommand — mnemonic-based identity registration.
//!
//! CLI-1 phase 2 (2026-05-08): exercises the same registration pipeline the
//! GUI app uses. Pavel directive: tolkicli MUST share crates с the mobile
//! app, so all crypto / wire / RPC logic comes from `tolki-client`'s shared
//! `registration` and `identity::mnemonic` modules. This file owns only:
//!
//! 1. Mnemonic acquisition: fresh 24-word BIP-39 phrase via
//!    `tolki_client::identity::Mnemonic::generate`, OR a user-supplied
//!    phrase passed через `--mnemonic`.
//! 2. Device-id persistence: 16-byte UUIDv7 stored at `~/.tolki/device-id.bin`
//!    so re-running `register` on the same machine hits the server's
//!    `device-id-already-registered` short-circuit (idempotent).
//! 3. Pretty-printed result + safety warning on freshly-generated mnemonics.
//!
//! Persistence beyond the device-id file (keychain mnemonic storage,
//! identity.toml) is intentionally out of scope here — separate later
//! task per the implementation plan.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use libp2p::{Multiaddr, PeerId};
use serde::Serialize;
use tolki_client::identity::{Mnemonic, MnemonicLength};
use tolki_client::registration::{
    register_identity_oneshot, RegistrationConfig, RegistrationError, RegistrationResult,
};
use tracing::info;
use uuid::Uuid;

/// Schema version of the on-disk `identity.toml`. Bump when the layout changes
/// in а backwards-incompatible way so older `tolkicli identity show` builds
/// can refuse to render and tell the user к upgrade.
const IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Top-level structure persisted к `~/.tolki/identity.toml`. The leading
/// `schema_version` makes the format self-describing for forward compat.
#[derive(Debug, Serialize)]
struct IdentityFile {
    schema_version: u32,
    identity: IdentitySection,
}

/// Inner `[identity]` table mirroring [`RegistrationResult`] plus the server
/// peer-id we registered against (so future `ping` calls can reuse it without
/// the user having к re-supply the long flag every time).
#[derive(Debug, Serialize)]
struct IdentitySection {
    user_id: String,
    device_id: String,
    registered_at_ms: i64,
    is_new_account: bool,
    server_peer_id: String,
}

/// Drive the `register` subcommand: acquire mnemonic + device-id, call into
/// the shared registration pipeline, pretty-print the outcome.
///
/// `mnemonic_opt = None` ⇒ generate а fresh 24-word phrase и print it
/// (с warning); `Some(phrase)` ⇒ use as-is and never echo the phrase back.
pub async fn run_register(
    peer_id: PeerId,
    multiaddr: Multiaddr,
    mnemonic_opt: Option<String>,
) -> Result<()> {
    let (phrase, generated) = acquire_mnemonic(mnemonic_opt)?;
    let device_id = load_or_create_device_id()?;
    info!(
        device_id = %Uuid::from_bytes(device_id),
        "register: dialing server"
    );

    let config = RegistrationConfig {
        server_peer_id: peer_id,
        server_multiaddrs: vec![multiaddr],
    };
    let result = register_identity_oneshot(&phrase, device_id, &config)
        .await
        .map_err(map_registration_error)?;

    print_result(&result, generated.then_some(phrase.as_str()));
    save_identity(&result, peer_id)?;
    Ok(())
}

/// Persist the registration outcome к `~/.tolki/identity.toml`.
///
/// Atomic-ish: serialises к `identity.toml.tmp` first then renames into
/// place so а Ctrl+C mid-write cannot leave а half-baked file. Refuses к
/// overwrite if the existing file holds а *different* user-id (a previous
/// identity is being clobbered — user must `tolkicli identity wipe` first).
/// Same `user_id` overwrites silently; re-register на same device is
/// idempotent on the server side too.
///
/// The mnemonic is **never** persisted here — that lives в keychain
/// territory (separate later task).
fn save_identity(result: &RegistrationResult, server_peer_id: PeerId) -> Result<()> {
    let path = identity_file_path()?;
    let new_user_id = Uuid::from_bytes(result.user_id);
    if let Some(existing) = read_existing_user_id(&path)? {
        if existing != new_user_id {
            bail!(
                "refusing to overwrite identity at {}: existing user_id {} \
                 differs from new user_id {} — run `tolkicli identity wipe` first",
                path.display(),
                existing,
                new_user_id,
            );
        }
    }

    let file = IdentityFile {
        schema_version: IDENTITY_SCHEMA_VERSION,
        identity: IdentitySection {
            user_id: new_user_id.to_string(),
            device_id: Uuid::from_bytes(result.device_id).to_string(),
            registered_at_ms: result.registered_at_ms,
            is_new_account: result.is_new_account,
            server_peer_id: server_peer_id.to_string(),
        },
    };
    write_identity_file(&path, &file)?;
    info!(path = %path.display(), "register: persisted identity");
    Ok(())
}

/// Resolve the canonical identity-file path: `${HOME}/.tolki/identity.toml`.
fn identity_file_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir()
        .context("could not determine $HOME — set HOME env var")?;
    Ok(home.join(".tolki").join("identity.toml"))
}

/// If `path` exists и parses as а valid identity file, return its `user_id`.
/// Returns `None` when the file is absent (first-time register). Surfaces
/// parse errors loudly — а corrupted file means we cannot prove the
/// no-clobber invariant.
fn read_existing_user_id(path: &Path) -> Result<Option<Uuid>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing identity at {}", path.display()))?;
    let value: toml::Value = toml::from_str(&text)
        .with_context(|| format!("failed to parse existing identity at {}", path.display()))?;
    let user_id_str = value
        .get("identity")
        .and_then(|t| t.get("user_id"))
        .and_then(|v| v.as_str())
        .with_context(|| {
            format!(
                "existing identity at {} is missing [identity].user_id",
                path.display()
            )
        })?;
    let user_id = Uuid::parse_str(user_id_str).with_context(|| {
        format!(
            "existing identity at {} has unparseable user_id {:?}",
            path.display(),
            user_id_str
        )
    })?;
    Ok(Some(user_id))
}

/// Serialize `file` к TOML и write atomically: tmp + rename. Ensures the
/// parent directory exists with mode 0700 first.
fn write_identity_file(path: &Path, file: &IdentityFile) -> Result<()> {
    let parent = path
        .parent()
        .context("identity-file path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    set_dir_mode_0700(parent)?;

    let body = toml::to_string_pretty(file)
        .context("failed to serialize identity к TOML")?;
    let banner = "# tolki identity — written by `tolkicli register`. Do not edit by hand.\n";
    let contents = format!("{banner}{body}");

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, contents.as_bytes())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// Either return the user-supplied phrase paired with `generated=false`, or
/// generate а fresh 24-word BIP-39 phrase paired with `generated=true`.
fn acquire_mnemonic(mnemonic_opt: Option<String>) -> Result<(String, bool)> {
    if let Some(phrase) = mnemonic_opt {
        let trimmed = phrase.trim().to_string();
        if trimmed.is_empty() {
            bail!("--mnemonic was provided but is empty");
        }
        return Ok((trimmed, false));
    }
    let mnemonic = Mnemonic::generate(MnemonicLength::TwentyFour)
        .context("failed to generate fresh BIP-39 mnemonic")?;
    Ok((mnemonic.phrase(), true))
}

/// Read `~/.tolki/device-id.bin` if present, else generate а fresh UUIDv7
/// и write it (creating `~/.tolki/` with mode 0700 on Unix).
fn load_or_create_device_id() -> Result<[u8; 16]> {
    let path = device_id_path()?;
    if path.exists() {
        return read_device_id_file(&path);
    }
    create_device_id_file(&path)
}

/// Resolve the canonical device-id path: `${HOME}/.tolki/device-id.bin`.
fn device_id_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir()
        .context("could not determine $HOME — set HOME env var")?;
    Ok(home.join(".tolki").join("device-id.bin"))
}

/// Read а previously-stored 16-byte device-id. Reject any other file size так
/// что corrupted state surfaces loudly instead of registering wrong identity.
fn read_device_id_file(path: &PathBuf) -> Result<[u8; 16]> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read device-id file {}", path.display()))?;
    if bytes.len() != 16 {
        bail!(
            "device-id file {} has wrong length: expected 16 bytes, got {}",
            path.display(),
            bytes.len()
        );
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes);
    Ok(id)
}

/// Generate а fresh UUIDv7, write it к disk, return the bytes.
fn create_device_id_file(path: &PathBuf) -> Result<[u8; 16]> {
    let parent = path
        .parent()
        .context("device-id path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    set_dir_mode_0700(parent)?;

    let id = Uuid::now_v7().into_bytes();
    fs::write(path, id)
        .with_context(|| format!("failed to write device-id к {}", path.display()))?;
    info!(path = %path.display(), "register: created new device-id");
    Ok(id)
}

/// On Unix, tighten the directory permissions to 0700 so other local users
/// cannot read the device-id file. No-op on non-Unix platforms.
#[cfg(unix)]
fn set_dir_mode_0700(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(dir)
        .with_context(|| format!("failed к stat {}", dir.display()))?
        .permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dir, perms)
        .with_context(|| format!("failed к chmod 0700 on {}", dir.display()))
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_dir: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Translate а [`RegistrationError`] к an [`anyhow::Error`] with а
/// human-readable surface string. Each variant gets its own context tag so
/// stderr output is grep-friendly for ops triage.
fn map_registration_error(err: RegistrationError) -> anyhow::Error {
    match err {
        RegistrationError::NetworkConnect(e) => {
            anyhow::anyhow!("network connect failed: {e}")
        }
        RegistrationError::Rpc(e) => anyhow::anyhow!("rpc failed: {e:?}"),
        RegistrationError::Server(e) => anyhow::anyhow!("server rejected: {e:?}"),
        RegistrationError::InvalidMnemonic => {
            anyhow::anyhow!("invalid mnemonic — phrase failed BIP-39 checksum / word-count")
        }
        RegistrationError::Signing(msg) => anyhow::anyhow!("signing failed: {msg}"),
    }
}

/// Pretty-print the result. `mnemonic_to_show` is `Some(phrase)` only when we
/// generated а fresh phrase ourselves — never when the user passed `--mnemonic`,
/// чтобы избежать echoing user-provided secrets к stdout.
fn print_result(result: &RegistrationResult, mnemonic_to_show: Option<&str>) {
    let user_id = Uuid::from_bytes(result.user_id);
    let device_id = Uuid::from_bytes(result.device_id);
    println!("✓ registered");
    println!("  user_id          {}", user_id);
    println!("  device_id        {}", device_id);
    println!("  registered_at_ms {}", result.registered_at_ms);
    println!("  is_new_account   {}", result.is_new_account);
    if let Some(phrase) = mnemonic_to_show {
        println!("  mnemonic         {}", phrase);
        println!();
        println!("⚠ SAVE THIS MNEMONIC — it cannot be recovered.");
    }
}
