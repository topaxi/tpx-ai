# Email Summary — Thunderbird extension plan

## Context

A tool that summarizes incoming email and surfaces a short digest, batched roughly
hourly, so the user can triage without reading everything.

Built **inside Thunderbird as a MailExtension (JS WebExtension)** rather than as a Rust
IMAP tool — this gives native, real-time access to new mail
(`messages.onNewMailReceived`) and reuses Thunderbird's own account credentials, so
there is no separate IMAP/SMTP config to manage.

This is the first non-Rust component in the repo. It lives outside the Cargo workspace;
the workspace `Cargo.toml` is untouched. It does **not** use the Rust `crates/llm`
crate — the extension talks to the LLM over HTTP directly, but mirrors that crate's
shape (`complete(messages)` with ollama/anthropic backends).

### Locked decisions

| Aspect | Choice |
|---|---|
| Integration | Thunderbird MailExtension (JavaScript, MV2) |
| Delivery | OS desktop notification per hourly batch (`notifications` API) |
| Provider default | Ollama local (`http://localhost:11434`); Anthropic selectable |
| Interval | In-extension `alarms` API, hourly (`periodInMinutes: 60`) |

> Note: the "external scheduler" (systemd/cron) idea does not apply to code running
> inside Thunderbird's process. The in-extension equivalent is the `alarms` API, which
> matches the intended hourly batching.

## Target behaviour

1. `messages.onNewMailReceived(folder, messages)` fires as mail arrives. We **enqueue**
   the new message IDs (+ folder) into `storage.local` (cheap hot path, no per-mail LLM
   call).
2. An hourly `alarms` alarm flushes the queue:
   - Load queued IDs, fetch each via `messages.getFull(id)` → headers (From/Subject/Date)
     + plain-text body part.
   - De-dupe, cap batch size, truncate each body (~2k chars), skip excluded folders.
   - Build one prompt, call the configured provider once → combined digest (one bullet
     per message: sender — one-line gist + "needs reply?" hint).
   - Show digest via `notifications.create`; clear the queue.
3. Empty queue on alarm → do nothing (no empty notification).

---

## Phases / Milestones

### Milestone 0 — Scaffolding & dev loop
**Goal:** an empty extension loads in Thunderbird and logs on new mail.
- `extensions/email-summary/manifest.json` (MV2): permissions
  `accountsRead, messagesRead, notifications, alarms, storage`; host_permissions
  `http://localhost/*`, `https://api.anthropic.com/*`; `background.scripts` + `options_ui`.
- `extensions/email-summary/background.js`: register `onNewMailReceived` and just
  `console.log` the folder + message count.
- Optional `package.json` with `web-ext` scripts (`run -t thunderbird`, `build`).
- **Done when:** temporary-install via `about:debugging` loads cleanly and logs on a
  received test email.

### Milestone 1 — Queue & batching
**Goal:** new mail is durably queued and an alarm flushes it on a schedule.
- `onNewMailReceived` → push `{id, folderId, ts}` into a `storage.local` pending queue
  (de-duped).
- On startup: `alarms.create("flush", { periodInMinutes: 60 })`; `alarms.onAlarm` →
  flush handler.
- Flush handler (no LLM yet): drain queue, `messages.getFull(id)`, extract headers +
  plain-text body (walk `MessagePart.parts`, fall back to stripped `text/html`), log the
  structured batch, clear queue.
- **Done when:** sending mail enqueues it; manually triggering the alarm logs the parsed
  batch and empties the queue.

### Milestone 2 — Provider layer
**Goal:** `complete(messages)` works against both backends.
- `providers.js` mirroring `crates/llm/src/lib.rs`:
  - `ollama(baseUrl, model)` → `POST {baseUrl}/api/chat`
    `{ model, messages, stream:false, think:false }` (the `think:false` flag is a known
    repo requirement for qwen3). Read `message.content`.
  - `anthropic(apiKey, model)` → `POST https://api.anthropic.com/v1/messages` with
    headers `x-api-key`, `anthropic-version: 2023-06-01`,
    `anthropic-dangerous-direct-browser-access: true`. Default `claude-haiku-4-5`.
- `summarize.js`: prompt builder turning `[{from,subject,date,body}]` into a system+user
  `messages` array (compact, one bullet per mail, flag action items). Single place to tune.
- **Done when:** a hardcoded sample batch returns a sensible digest from Ollama.

### Milestone 3 — Wire flush → digest → notification
**Goal:** the full real path produces a desktop notification.
- Flush handler calls `summarize` → `provider.complete` → `notifications.create`
  (title e.g. "📬 5 new — hourly digest", body = bullets).
- Truncate bodies / cap batch to bound prompt size and keep local models fast.
- Never log full bodies.
- **Done when:** a received email produces, on the next alarm, a digest notification and
  an emptied queue.

### Milestone 4 — Options page
**Goal:** configurable without editing code.
- `options/options.html` + `options.js` backed by `storage.local`: provider
  (ollama|anthropic), model, Ollama URL, Anthropic API key, interval minutes, max
  messages/batch, excluded folders.
- Background reads settings; alarm period updates when interval changes.
- **Done when:** switching provider/model in the UI changes the next digest's source.

### Milestone 5 — Robustness & docs
**Goal:** clear failure UX and a documented install.
- On provider/network error: show a clear error notification and **preserve** the queue
  for retry (don't drop messages).
- `README.md`: dev + packaged install, and the **Ollama CORS** requirement
  (`OLLAMA_ORIGINS="moz-extension://*"`).
- Update root `CLAUDE.md` with an `extensions/` section (provider defaults, CORS note,
  `web-ext` dev/build commands), mirroring the existing "Neovim Plugin" doc style.
- **Done when:** stopping Ollama mid-flush yields an error notification with the queue
  intact, and a fresh install works from the README alone.

---

## Key gotchas

- **Ollama CORS:** extension `Origin` is `moz-extension://…`; Ollama rejects it unless run
  with `OLLAMA_ORIGINS="moz-extension://*"` (or `*` for dev). Document prominently;
  surface a clear error if the fetch fails.
- **Anthropic from extension:** requires the `anthropic-dangerous-direct-browser-access:
  true` header + the host permission.
- **Body extraction:** `getFull` returns a nested `MessagePart` tree — walk `parts`.
- **Privacy:** default provider is local Ollama; never log full bodies.
- **Manifest version:** MV2 is the reliable target for background event handling on
  Thunderbird 115/128 ESR.

## Files

```
extensions/email-summary/
  manifest.json
  background.js          # onNewMailReceived queue + alarms flush
  providers.js           # ollama / anthropic complete()
  summarize.js           # prompt builder
  options/options.html
  options/options.js
  README.md
  package.json           # optional web-ext dev/build
```

## Verification (end-to-end)

1. **Load (dev):** Thunderbird → `about:debugging` → "Load Temporary Add-on" →
   `extensions/email-summary/manifest.json` (or `web-ext run -t thunderbird`).
2. **Provider up:** `ollama serve` with `OLLAMA_ORIGINS="moz-extension://*"`, model
   pulled (repo default `qwen2.5-coder`). Set provider/model in options.
3. **Manual flush:** in the background console, call the flush fn directly (or set
   `alarms.create({delayInMinutes: 0.1})`) — confirm a notification with one bullet per
   queued message.
4. **Real path:** send yourself a test email → confirm it enqueues (log queue length) →
   next alarm produces a notification and clears the queue.
5. **Anthropic path:** switch provider in options, set API key, repeat step 3 (validates
   the browser-access header + host permission).
6. **Failure UX:** stop Ollama, trigger flush → clear error notification, queue preserved.
```
