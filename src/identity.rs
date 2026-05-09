//! `identity` subcommand group — local-only inspection / management of the
//! `~/.config/tolki/` state directory.
//!
//! CLI-1 phase 3 (2026-05-08): purely synchronous, no network I/O. Reads the
//! files written by `register` (device-id.bin, identity.toml) and prints them
//! back, or wipes them on demand. The mnemonic lives в keychain (separate
//! later task) — `wipe` does NOT touch it.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

/// Print currently-persisted identity. Output shape:
///
/// - Both files present → full identity card.
/// - Only `device-id.bin` present → device-id + hint to register.
/// - Neither present → "no identity registered yet" prompt.
///
/// Errors only on I/O failures or а malformed `identity.toml`.
pub fn run_show() -> Result<()> {
    let device_id_path = device_id_path()?;
    let identity_path = identity_file_path()?;

    let device_id = read_device_id_if_present(&device_id_path)?;
    let identity = read_identity_if_present(&identity_path)?;

    match (identity, device_id) {
        (Some(id), _) => print_full_identity(&id, &identity_path),
        (None, Some(dev)) => print_device_id_only(dev, &device_id_path),
        (None, None) => println!("no identity registered yet — run `tolkicli register`"),
    }
    Ok(())
}

/// Delete `~/.config/tolki/identity.toml` и `~/.config/tolki/device-id.bin`. With
/// `yes = false` prompts on stdin and aborts unless the user types
/// `y` / `yes`. The `~/.config/tolki/` directory itself is left in place — empty
/// directories are harmless and removing it would race with concurrent
/// state writes.
pub fn run_wipe(yes: bool) -> Result<()> {
    let device_id_path = device_id_path()?;
    let identity_path = identity_file_path()?;

    if !yes && !confirm_wipe(&[&identity_path, &device_id_path])? {
        println!("aborted — no files deleted");
        return Ok(());
    }

    let removed_identity = remove_if_exists(&identity_path)?;
    let removed_device = remove_if_exists(&device_id_path)?;

    if !removed_identity && !removed_device {
        println!("nothing to wipe — no identity files exist");
    } else {
        if removed_identity {
            println!("removed {}", identity_path.display());
        }
        if removed_device {
            println!("removed {}", device_id_path.display());
        }
        println!("note: mnemonic in keychain (if any) was NOT touched");
    }
    Ok(())
}

/// Resolve `${HOME}/.config/tolki/identity.toml`. Mirrors the helper в register.rs;
/// kept here so the `identity` module is self-contained for sync callers.
fn identity_file_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir()
        .context("could not determine $HOME — set HOME env var")?;
    Ok(home.join(".config").join("tolki").join("identity.toml"))
}

/// Resolve `${HOME}/.config/tolki/device-id.bin`. Same path as register.rs's helper;
/// duplicated rather than cross-module-imported so identity stays а leaf.
fn device_id_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir()
        .context("could not determine $HOME — set HOME env var")?;
    Ok(home.join(".config").join("tolki").join("device-id.bin"))
}

/// Returns `Some(uuid)` if `~/.config/tolki/device-id.bin` exists и is а valid
/// 16-byte file. Surfaces I/O errors loudly; missing file is `None`.
fn read_device_id_if_present(path: &Path) -> Result<Option<Uuid>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() != 16 {
        bail!(
            "device-id file {} has wrong length: expected 16 bytes, got {}",
            path.display(),
            bytes.len()
        );
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&bytes);
    Ok(Some(Uuid::from_bytes(arr)))
}

/// Parsed contents of `identity.toml` for display purposes. Mirrors fields
/// the register-side `IdentityFile` writes. Kept а separate type so we can
/// tolerate forward-compat additions (extra fields ignored here).
struct ParsedIdentity {
    schema_version: u32,
    user_id: String,
    device_id: String,
    registered_at_ms: i64,
    is_new_account: bool,
    server_peer_id: String,
}

/// Read и parse `identity.toml`. Returns `None` when the file is absent so
/// the caller can fall back к device-id-only display.
fn read_identity_if_present(path: &Path) -> Result<Option<ParsedIdentity>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value = toml::from_str(&text)
        .with_context(|| format!("failed to parse {} as TOML", path.display()))?;
    Ok(Some(extract_identity(&value, path)?))
}

/// Pull the documented fields out of the parsed TOML tree. Each missing
/// field produces а labeled error referencing the offending file path.
fn extract_identity(value: &toml::Value, path: &Path) -> Result<ParsedIdentity> {
    let schema_version = value
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .with_context(|| format!("{} missing schema_version", path.display()))?
        as u32;
    let identity = value
        .get("identity")
        .with_context(|| format!("{} missing [identity] table", path.display()))?;
    let s = |k: &str| -> Result<String> {
        identity
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .with_context(|| format!("{} missing identity.{}", path.display(), k))
    };
    let i = |k: &str| -> Result<i64> {
        identity
            .get(k)
            .and_then(|v| v.as_integer())
            .with_context(|| format!("{} missing identity.{}", path.display(), k))
    };
    let b = |k: &str| -> Result<bool> {
        identity
            .get(k)
            .and_then(|v| v.as_bool())
            .with_context(|| format!("{} missing identity.{}", path.display(), k))
    };
    Ok(ParsedIdentity {
        schema_version,
        user_id: s("user_id")?,
        device_id: s("device_id")?,
        registered_at_ms: i("registered_at_ms")?,
        is_new_account: b("is_new_account")?,
        server_peer_id: s("server_peer_id")?,
    })
}

/// Render the full identity card к stdout. Aligned columns match the
/// register subcommand's success output for visual consistency.
fn print_full_identity(id: &ParsedIdentity, path: &Path) {
    println!("identity ({})", path.display());
    println!("  schema_version   {}", id.schema_version);
    println!("  user_id          {}", id.user_id);
    println!("  device_id        {}", id.device_id);
    println!("  registered_at_ms {}", id.registered_at_ms);
    println!("  is_new_account   {}", id.is_new_account);
    println!("  server_peer_id   {}", id.server_peer_id);
}

/// When only `device-id.bin` is present: print it и hint the user that
/// they have not registered yet.
fn print_device_id_only(device_id: Uuid, path: &Path) {
    println!("partial state — device-id only ({})", path.display());
    println!("  device_id        {}", device_id);
    println!();
    println!("no registration on file — run `tolkicli register` to associate");
    println!("this device-id with а user identity.");
}

/// Interactive y/N confirmation. Lists exactly which paths will be removed
/// so the user knows what's about к happen. Returns `Ok(true)` only on а
/// case-insensitive `y` или `yes`. Anything else (including EOF) aborts.
fn confirm_wipe(paths: &[&Path]) -> Result<bool> {
    println!("about to delete the following local files:");
    for p in paths {
        println!("  {}", p.display());
    }
    println!("the mnemonic in keychain (if any) will NOT be touched.");
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

/// `fs::remove_file` that returns `Ok(false)` for а missing file и
/// `Ok(true)` when something was actually removed. All other I/O errors
/// surface as `Err`.
fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("failed to delete {}", path.display()))),
    }
}
