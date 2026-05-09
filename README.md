# tolkicli

Терминальный клиент Tolki. Должен уметь всё то же что и GUI-приложение, плюс быть удобным для скриптов / автоматизации.

**Где живёт:** sibling crate с `tolki-client` / `tolki-wire` → [github.com/tolkichat/tolkicli](https://github.com/tolkichat/tolkicli).

**Принцип:** делит код с GUI через `tolki-client` lib (Cargo feature `cli` без ASR/LLM). CLI тестит ровно тот же код что использует GUI — так баги ловятся в обе стороны.

> Pavel directive 2026-05-08: «Толки CLI должен использовать тот же код. И те же библиотеки. Что использует и наша программа с GUI» — иначе testing through tolkicli не доказывает ничего о GUI behaviour.

---

## Что Pavel назвал

- Зарегистрироваться (создать новый аккаунт, mnemonic)
- Создать аккаунт от username (захватить хендл)
- Сменить юзернейм
- Подключаться к сети
- Отправлять сообщения

---

## Зачем

| Use case | Как |
|----------|-----|
| Smoke-тест wire-протокола | `tolkicli ping` |
| Регистрация identity | `tolkicli register` (свежая BIP-39 mnemonic + UUIDv7 device-id, register-identity RPC) |
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

## Что готово сейчас (Phase 1 + начало Phase 2)

| Команда | Что делает | Статус |
|---------|-----------|--------|
| `tolkicli ping` | Bidi-стрим ping/pong. RTT в реальном времени, summary на выходе. Smoke-тест wire-протокола | ✅ |
| `tolkicli register` | Сгенерировать 24-word BIP-39 mnemonic (или импортировать через `--mnemonic`), сохранить device-id (UUIDv7) в `~/.tolki/device-id.bin`, отправить `tolki:registration@1.0.0/registration/register-identity` RPC, сохранить результат в `~/.tolki/identity.toml`. Делит весь pipeline с GUI через `tolki_client::registration::register_identity_oneshot` | ✅ |
| `tolkicli identity show` | Показать содержимое `~/.tolki/identity.toml` + device-id (или сообщить что identity не зарегистрирован) | ✅ |
| `tolkicli identity wipe [--yes]` | Удалить `~/.tolki/identity.toml` + `device-id.bin` (с подтверждением, либо `--yes` для скриптов). Mnemonic в keychain отдельный | ✅ |

**Note:** флаги `--server-peer-id` / `--server-multiaddr` живут per-subcommand (`tolkicli ping --server-peer-id ...` / `tolkicli register --server-peer-id ...`), а не на top-level. `tolkicli identity show/wipe` — pure filesystem, серверные флаги не нужны.

### Примеры

```bash
# Ping
tolkicli ping \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1 \
  [--interval-ms 1000] [--duration-s 30]

# Свежая identity (печатает mnemonic — сохрани!)
tolkicli register \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1

# Восстановление существующей identity
tolkicli register --mnemonic "abandon abandon ... about" \
  --server-peer-id <PEER_ID> \
  --server-multiaddr /ip4/<IP>/udp/<PORT>/quic-v1

# Inspect persisted state
tolkicli identity show
# ✓ identity registered
#   user_id          <uuid>
#   device_id        <uuid>
#   registered_at_ms 1715000000000
#   is_new_account   true
#   server_peer_id   12D3KooW...

# Cleanup
tolkicli identity wipe --yes
```

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

## Полный план команд (по группам)

### 🔑 Идентичность

| Команда | Что делает |
|---------|-----------|
| `tolkicli register` | ✅ Сгенерировать новую mnemonic (или import через `--mnemonic`), persist device-id + identity.toml |
| `tolkicli login --mnemonic "..."` | Восстановить существующую идентичность из mnemonic (без новой регистрации device) |
| `tolkicli logout` | Очистить локальное состояние (оставить mnemonic в keychain) |
| `tolkicli identity show` | ✅ Показать текущий handle / IK fingerprint / device-id |
| `tolkicli identity export` | Показать mnemonic для бэкапа (с подтверждением) |
| `tolkicli identity wipe` | ✅ Удалить идентичность из keychain + `~/.tolki/` |

### 👤 Username (handle)

| Команда | Что делает |
|---------|-----------|
| `tolkicli username claim <handle>` | Захватить Foundation handle (зарегистрировать `pavel-handle:` за нашим IK) |
| `tolkicli username show` | Показать текущий handle |
| `tolkicli username change <new-handle>` | Сменить handle (Phase 2 — через NFT-трансфер) |
| `tolkicli username available <handle>` | Проверить свободен ли handle |

### 🌐 Сеть и подключение

| Команда | Что делает |
|---------|-----------|
| `tolkicli connect` | Подключиться к серверу (config или флаги) |
| `tolkicli disconnect` | Отключиться |
| `tolkicli status` | Состояние подключения, server health, sync state |
| `tolkicli ping` | ✅ RTT smoke-тест |
| `tolkicli sync [--from <seq>]` | Принудительно re-sync events |

### 💬 Сообщения

| Команда | Что делает |
|---------|-----------|
| `tolkicli send <handle\|chat-id> "<текст>"` | Отправить текстовое сообщение |
| `tolkicli send-voice <handle\|chat-id> <audio.opus>` | Отправить голосовое (готовый opus-файл) |
| `tolkicli list-chats [--format json]` | Список всех чатов |
| `tolkicli list-messages <chat-id> [--limit N --since <ts>]` | История сообщений |
| `tolkicli watch [<chat-id>]` | Live-подписка — print новых сообщений по мере прихода |

### 👥 Контакты + инвайты (consent-based)

**Важно:** добавить контакт можно только через mutual consent. Два пути:
- **Async invitation** — знаешь только handle. Отправляешь invitation, recipient принимает. Анти-спам: rate-limit + reputation tier. См. `Contact-Consent-Anti-Spam.md`.
- **QR fast-track** — physical proximity (или secure share). Сканируешь QR → instant mutual add (без invitation queue). См. `QR-Contact-Add.md`.

| Команда | Что делает |
|---------|-----------|
| `tolkicli invite <handle> [--message "..."]` | Отправить async invitation. Сервер проверяет rate-limit + reputation tier |
| `tolkicli invitations` | Pending invitations к тебе |
| `tolkicli invite-respond <id> accept\|decline\|ignore\|block` | Ответить на invitation |
| `tolkicli sent-invitations` | Мои отправленные invitations + статус |
| `tolkicli invitation-budget` | Сколько invitations осталось сегодня + reputation tier |
| `tolkicli inbox-policy [show\|set <policy>]` | Default: `invitation-required`. Альтернативы: `open`, `friends-of-friends` |
| `tolkicli contact list` | Список accepted контактов |
| `tolkicli contact remove <handle>` | Удалить из контактов (revoke consent) |
| `tolkicli contact block <handle>` | Заблокировать. Future invitations silently rejected |
| `tolkicli contact unblock <handle>` | Разблокировать |

### 📷 QR-код для быстрого добавления

**Принцип:** показал QR — отсканировал — instant mutual contact. Криптографически подтверждено через подписи sub-key. Работает P2P без сервера или через сервер (если нет физического присутствия).

| Команда | Что делает |
|---------|-----------|
| `tolkicli qr generate [--expires-in-s 300] [--output qr.png]` | Создать QR-тикет, подписанный твоим IK_qr. Default TTL 5 мин, ASCII в терминале или PNG-файл |
| `tolkicli qr redeem [--image qr.png \| --url tolki:invite:...]` | Сканировать QR, верифицировать подпись, добавиться к контактам |
| `tolkicli qr-redemptions` | Кто отсканировал мой QR (Alice's side notification) |

### 🔐 Безопасность ключей

**Принцип:** один master-ключ хранится в Secure Enclave / keychain (cold), отдельные sub-keys для каждой задачи (QR, invitations, messaging) — могут быть rotated при подозрении на leak без потери identity. См. `Key-Hierarchy-And-Rotation.md`.

| Команда | Что делает |
|---------|-----------|
| `tolkicli security keys` | Показать current sub-keys + last rotation timestamps |
| `tolkicli security rotate <purpose>` | Ротация sub-key (`qr` / `invitation` / `messaging`). Подписывает новый ключ через IK_master, сервер инвалидирует старый |
| `tolkicli security rotate --all` | Параноид-режим: ротация всех sub-keys одновременно |
| `tolkicli security devices` | Список активных устройств (per-device IK) |
| `tolkicli security revoke-device <device-id>` | Отозвать device-key потерянного устройства + force ротацию hot sub-keys |
| `tolkicli security audit` | Audit log: история ротаций, accept/decline ratio, suspicious events |

### 🪪 Multi-account (профили)

**Принцип:** каждый аккаунт — own mnemonic → own master-key → own sub-keys. Полная изоляция. По дефолту один профиль; для multi-account используй `--profile`.

| Команда | Что делает |
|---------|-----------|
| `tolkicli profile list` | Все профили в `~/.tolki/profiles/` |
| `tolkicli profile create <name>` | Новый профиль (автоматически запустит `register` flow) |
| `tolkicli profile switch <name>` | Переключиться (или `--profile <name>` к любой команде) |
| `tolkicli profile remove <name>` | Удалить профиль (с confirmation) |

### 🎙️ Push-to-talk (Phase 3+)

| Команда | Что делает |
|---------|-----------|
| `tolkicli ptt <chat-id>` | Открыть микрофон, стримить voice-chunks (bidi) |
| `tolkicli ptt-listen <chat-id>` | Слушать live-стрим из чата |

### 📦 Модули и типы

| Команда | Что делает |
|---------|-----------|
| `tolkicli module install <canonical-name>` | Установить пользовательский тип/модуль через registry-proxy |
| `tolkicli module list` | Установленные модули + capabilities |
| `tolkicli module remove <canonical-name>` | Удалить + revoke capabilities |
| `tolkicli module publish <path>` | Опубликовать свой WIT-пакет в registry (через wkg + IK подпись) |
| `tolkicli introspect` | Скачать WIT-spec прямо с сервера через `tolki:introspect@1.0.0/get-wit-document` (после ship'а server-side codegen pipeline) |

### 🐛 Диагностика

| Команда | Что делает |
|---------|-----------|
| `tolkicli debug envelope --hex <bytes>` | Декодировать wire envelope (frame type, request-id, method-id, payload) |
| `tolkicli debug method-id <canonical-name>` | Вычислить method-id по имени |
| `tolkicli debug schema-cache list` | Что в локальном кэше схем |
| `tolkicli debug schema-cache invalidate <type-id>` | Сбросить кэш для конкретного типа |

### ⚙️ Конфигурация

| Команда | Что делает |
|---------|-----------|
| `tolkicli config show` | Показать текущий конфиг |
| `tolkicli config set <key> <value>` | Поменять параметр (server-address, log-level, ...) |
| `tolkicli config reset` | Сброс к defaults |

### 💾 Бэкап / миграция

| Команда | Что делает |
|---------|-----------|
| `tolkicli backup --output <path>` | Экспорт всего state (mnemonic не включён — он в keychain отдельно) |
| `tolkicli restore --input <path>` | Импорт state на новом устройстве |
| `tolkicli migrate --to <new-mnemonic>` | Перенести идентичность на новую mnemonic (через Foundation rebinding) |

---

## Дополнительные предложения

| Команда | Зачем |
|---------|-------|
| `tolkicli search <query>` | Поиск по тексту сообщений локально |
| `tolkicli debug method-id <name>` | Дебаг Phase 2 хешей — показать какой ID получится для имени |
| `tolkicli --format json/ndjson` | Скрипты и автоматизация (`jq`-pipeline) |
| `tolkicli --profile <name>` | Multi-account — несколько идентичностей на одной машине |
| `tolkicli daemon start/stop` | Phase 5 — фоновый процесс держит соединение, CLI вызовы через Unix-сокет (быстрее на скриптах в цикле) |

---

## Use cases для автоматизации

```bash
# Smoke-тест в CI
tolkicli register --mnemonic "$TEST_MNEMONIC" \
  --server-peer-id "$TEST_PEER_ID" --server-multiaddr "$TEST_MULTIADDR"
tolkicli send $TEST_RECIPIENT "smoke test $(date)"
tolkicli watch --format ndjson | head -1 | jq -e '.delivered == true'

# Бэкап чатов
for chat in $(tolkicli list-chats --format json | jq -r '.[].id'); do
    tolkicli list-messages $chat --format ndjson > backup-$chat.ndjson
done

# Эхо-бот
tolkicli watch --format ndjson | while read msg; do
    [[ $(echo $msg | jq -r .text) == "!ping" ]] && \
        tolkicli send "$(echo $msg | jq -r .chat_id)" "pong from $(hostname)"
done
```

---

## Что блокирует развитие

1. **Foundation IK_master keypair** (TR-IK-MASTER-001 на TS) — нужен для embed pubkey в client binary, чтобы verify подписи Foundation handle bindings. Блокирует `username claim` / `module install`.

2. **Foundation WIT packages** — для каждой группы команд нужен свой WIT-пакет:
   - `tolki:chat@1.0.0` — для send-message / list-chats / watch (draft в `Chat-WIT-Design-v1.md`, awaiting ratification)
   - `tolki:contacts@1.0.0` — для contact list / inbox-policy
   - `tolki:invitations@1.0.0` — для invite / invite-respond / qr / qr-redemptions
   - `tolki:identity@1.0.0/keys` — для security rotate / sub-key derivation
   - `tolki:ptt@1.0.0` — для push-to-talk
   - `tolki:introspect@1.0.0` — для introspect (TS Phase 1+2 codegen pipeline в работе)
   - Уже есть: `tolki:registration`, `tolki:sync`, `tolki:profile`, `tolki:ping`, `tolki:registry-proxy`

3. **TR-LEGACY-RETIRE-002** (TC, ~3-5 дней) — удаление старого `api_client/` (gRPC over tquic) из tolki-client. После этого можно мигрировать chat / push2talk на новые wire-протокол-методы.

4. **Key Hierarchy ship первым** — QR + invitations используют sub-keys (IK_qr, IK_invitation). Сначала Key-Hierarchy-And-Rotation.md имплементируется (KH-1..KH-10), потом QR + invitations переключаются с IK_master на sub-keys.

---

## Порядок реализации

| Фаза | Длительность | Команды |
|------|-------------|---------|
| **Phase 1** ✅ | — | `ping` |
| **Phase 2 (early)** ✅ | — | `register`, `identity show/wipe` |
| **Phase 2 (rest)** | ~5 недель | `login`, `logout`, `identity export`, `username claim/show/change/available`, `connect/status/sync`, `send/list-chats/list-messages/watch`, `config show/set/reset`, **Key Hierarchy** (`security keys/rotate/audit`) |
| **Phase 3** | ~3 недели | invitations (`invite`, `invitations`, `invite-respond`, `inbox-policy`), QR (`qr generate/redeem/redemptions`), `contact list/block/unblock`, `debug envelope/method-id/schema-cache`, `--format json/ndjson`, `profile list/create/switch`, `introspect` (когда TS pipeline ready) |
| **Phase 4** | ~3 недели | `send-voice`, `ptt`, `module install/publish`, `search`, `backup/restore`, `migrate`, `security devices/revoke-device` |
| **Phase 5 (опц.)** | ~1 неделя | `daemon start/stop`, periodic auto key rotation policy |

---

## Канонический design-документ

Полный design + архитектура + use cases + sub-task breakdown:
[`Tolki-CLI-Design.md`](https://github.com/tolkichat/tolki-docs/blob/main/raw/Products/Tolki/Tolki-CLI-Design.md) в tolki-docs.

Этот README — quick command reference + текущий статус. Для глубокого design rationale (open questions, architectural trade-offs, daemon-mode planning) — смотри Tolki-CLI-Design.md.

---

## Лицензия

Часть проекта Tolki ([github.com/tolkichat](https://github.com/tolkichat)).
