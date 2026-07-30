# Free Token Energy

Free Token Energy is a local-first desktop AI gateway built with Tauri 2, Rust,
SQLite, and a dependency-light webview. It presents several provider accounts
through one OpenAI-compatible loopback API and can select an available route
for each request.

## What works

- OpenRouter, Groq, Anthropic, Google Gemini, Mistral, NVIDIA NIM, and Cerebras
- Native request and stream normalization for Anthropic Messages and Gemini
  `generateContent`
- OpenAI-compatible chat, completions, responses, model listing, and SSE
- Atomic sliding-window request reservations that survive application restarts
- Measured local request totals, token usage, latency, and provider outcomes
- A desktop dashboard, provider setup, chat playground, activity log, and
  persistent proxy-port settings

The model catalog uses current provider model IDs. Published free-tier limits
are tracked locally where a provider documents them; account-specific or
unknown limits are shown as unknown and receive a neutral routing score. The
application does not invent benchmark, latency, usage, or quota data.

## Run locally

Requirements: a current Rust toolchain, Node.js 22 or later, and the
platform-specific Tauri prerequisites.

```sh
npm ci
npm test
npm run dev
```

The proxy starts on `127.0.0.1:1337` by default. Its port can be changed from
Settings without restarting the desktop application.

## API surface

- `GET /v1/models`
- `POST /v1/completions`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `POST /v1/messages`
- `POST /v1beta/models/{model}:generateContent`
- `POST /v1beta/models/{model}:streamGenerateContent`

Use `model: "auto"` to let the router choose among configured, capable models,
or use a public model ID returned by `/v1/models`.

## Routing

Eligible routes must have a configured key, support the requested model and
capabilities, and have local quota remaining. They are ranked with:

```text
0.35 × documented-limit headroom
+ 0.30 × measured evaluation score
+ 0.20 × declared capability breadth
+ 0.15 × observed latency
```

Unknown quota, evaluation, and latency inputs are neutral rather than
fabricated. Request counts are reserved atomically before dispatch; token usage
is recorded when it becomes available.

## Privacy and security

Keys and request metadata stay in the local app database, there is no
telemetry, and the proxy listens only on IPv4 loopback. API keys are protected
by app-directory and file permissions on Unix but are not encrypted at rest.
Read [SECURITY.md](SECURITY.md) before using valuable provider credentials.

## Development

```sh
npm run check
npm run test:rust
cargo check --all-targets --all-features --manifest-path src-tauri/Cargo.toml
```

Provider research notes and machine-readable policies are kept in
`research/provider-gateways/`. Local cloned reference repositories under that
directory are intentionally ignored rather than vendored.
