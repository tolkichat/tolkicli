# tolkicli

Terminal-клиент Tolki — единый Rust-binary, делящий весь wire/crypto/transport-stack с GUI-приложением через `tolki-client` lib (Cargo feature `cli` без ASR/LLM).

> Pavel directive 2026-05-08: «Толки CLI должен использовать тот же код. И те же библиотеки. Что использует и наша программа с GUI» — иначе testing through tolkicli не доказывает ничего о GUI behaviour.

---

## Зачем

| Use case | Как |
|----------|-----|
| Smoke-тест wire-протокола | `tolkicli ping` |
| Регистрация identity | `tolkicli register` (генерит свежую BIP-39 mnemonic + UUIDv7 device-id, отправляет register-identity RPC) |
| Восстановление identity | `tolkicli register --mnemonic "<24 words>"` |
| Headless usage | Скрипты + автоматизация (CI smoke-тесты, бэкапы, эхо-боты) |
| Pavel quick-validation | Тестировать новый функционал не запуская iOS/macOS app |

CLI **не end-user productivity** — для повседневной работы UI-app остаётся primary. Это **power-user / developer / automation** инструмент.

---

## Build & запуск

```bash
cd /path/to/tolki/tolkicli   # sibling crate с tolki-client / tolki-wire
cargo build --release         # ~6 MB ort-free binary
./target/release/tolkicli --help
```

Зависит от `tolki-client = { path = "../tolki-client", default-features = false, features = ["cli"] }` — feature `cli` отключает ASR/LLM (ort/onnxruntime/whisper/llama), чтобы binary остался lightweight.

---

## Subcommands

### `tolkicli ping`

Bidi-стрим `tolki:ping@1.0.0/ping/ping-pong`. RTT по каждому pong'у, summary на выходе.

```bash
tolkicli ping \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1 \
  [--interval-ms 1000] \
  [--duration-s 30]
```

**Доказывает:** transport (libp2p QUIC + Noise XX) + wire-protocol substream работают.

### `tolkicli register`

Генерирует свежую 24-словную BIP-39 mnemonic (через `tolki_client::identity::Mnemonic::generate(MnemonicLength::TwentyFour)` — тот же путь что и в GUI), создаёт UUIDv7 device-id, отправляет `tolki:registration@1.0.0/registration/register-identity` RPC.

```bash
# Свежая identity (печатает mnemonic — сохрани!)
tolkicli register \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1

# Восстановление существующей identity
tolkicli register --mnemonic "abandon abandon ... about" \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1
```

**Persistence:**
- `~/.tolki/device-id.bin` — 16-byte UUIDv7, persisted (mode 0700 на Unix). Idempotent re-registration на том же устройстве работает через server-side `device-id-already-registered` short-circuit.
- `~/.tolki/identity.toml` — `{user_id, device_id, registered_at_ms, is_new_account, server_peer_id}` после успешной регистрации (atomic write через `.tmp` + rename).

Mnemonic печатается ОДИН раз в stdout (только при свежей генерации, никогда при `--mnemonic`). Keychain integration — отдельный TODO.

### `tolkicli identity show`

Печатает содержимое `~/.tolki/identity.toml` + device-id.bin. Не требует серверных флагов.

```bash
tolkicli identity show
# ✓ identity registered
#   user_id          <uuid>
#   device_id        <uuid>
#   registered_at_ms 1715000000000
#   is_new_account   true
#   server_peer_id   12D3KooW...
```

### `tolkicli identity wipe [--yes]`

Удаляет `~/.tolki/identity.toml` + device-id.bin. Mnemonic в keychain не трогает. Без `--yes` спрашивает confirmation, перечисляя точные пути.

---

## Локальный state (`~/.tolki/`)

```
~/.tolki/
├── identity.toml        # post-register state (user_id, device_id, server peer-id, ...)
└── device-id.bin        # 16 bytes, UUIDv7, generated at first register
```

Директория создаётся с режимом `0700` на Unix. Mnemonic **никогда не сохраняется** на диске в plaintext — будет в OS keychain (Apple Keychain / Linux Secret Service / Windows Credential Manager) когда keychain-integration будет shipped.

---

## Принципы

1. **Делит код с GUI.** Все RPC / crypto / transport — через `tolki_client::registration::*`, `tolki_client::wire_client::*`, `tolki_client::identity::*`. tolkicli owns только argparse + persistence + pretty-print.
2. **Bytes-only wire.** TLV-encoded WIT records через method-addressed envelope (Wire-Protocol-v2 § 3.5). Никакого JSON.
3. **One binary, no daemon.** Каждый вызов открывает свежее libp2p QUIC-соединение. Daemon mode (`tolki-clid`) опционально в Phase 5.
4. **Machine-readable output.** `--format json` / `--format ndjson` будет на каждом subcommand'е (Phase 4) — pipeable в `jq`.

---

## Roadmap

| Phase | Status | Subcommands |
|-------|--------|-------------|
| 1 | ✅ shipped | `ping` |
| 2 (early) | ✅ shipped | `register`, `identity show/wipe` |
| 2 (rest) | 🔜 | `login`, `username claim/show/change/available`, `connect/status/sync`, `send`, `list-chats/list-messages/watch`, `config show/set/reset` |
| 3 | 🔜 | invitations + QR + contacts + debug envelope/method-id/schema-cache, `--format json/ndjson`, profiles |
| 4 | 🔜 | `send-voice`, `ptt`, `module install/publish`, `search`, `backup/restore`, `migrate` |
| 5 (опц.) | 🔜 | `daemon start/stop` + автоматическая sub-key rotation policy |

Полный design + open questions: [`Tolki-CLI-Design.md`](https://github.com/tolkichat/tolki-docs/blob/main/raw/Products/Tolki/Tolki-CLI-Design.md) в tolki-docs.

### Запланированный `introspect` (после TS ship Phase 1+2 codegen pipeline)

```bash
tolkicli introspect --server-peer-id <X> --server-multiaddr <Y>
# → дёргает tolki:introspect@1.0.0/get-wit-document, печатает .wit
```

Позволит любому клиенту скачать WIT-spec прямо с сервера через wire-protocol — не нужен out-of-band доступ к design-docs / git-репозиторию.

---

## Лицензия

Часть проекта Tolki ([github.com/tolkichat](https://github.com/tolkichat)).
