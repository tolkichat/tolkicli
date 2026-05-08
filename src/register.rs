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
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use libp2p::{Multiaddr, PeerId};
use tolki_client::identity::{Mnemonic, MnemonicLength};
use tolki_client::registration::{
    register_identity_oneshot, RegistrationConfig, RegistrationError, RegistrationResult,
};
use tracing::info;
use uuid::Uuid;

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
