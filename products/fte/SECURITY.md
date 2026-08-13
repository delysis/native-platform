# Security

Free Token Energy is local-first, but local-first is not the same as encrypted.

## Local data

- Provider API keys are stored through the platform credential service. Fresh
  SQLite databases never create plaintext key storage. The app does not import
  legacy plaintext stores: it rejects them before schema mutation and tells the
  operator to move or remove the unsupported database.
- Use revocable, least-privilege provider keys. Platform credential storage
  reduces exposure but does not make a compromised signed-in account safe.
- Request logs contain provider ID, public model ID, token count, latency,
  status, and timestamp. Prompts, responses, profile values, and API keys are
  not logged.
- The signup profile accepts only name, email, and a non-secret password hint.
- Local GGUF configuration stores the canonical model path and optional
  expected SHA-256 in the private application database. The webview receives
  only the filename and readiness detail, not the full path.
- The application does not include telemetry.

## Local interfaces

The `fte-loopback` server is disabled until explicitly started. It
binds only loopback addresses, validates `Host` and configured origins, applies
bounded request/stream limits, and requires a random app-private bearer token.
Hosted provider credentials never cross that interface.

## Provider traffic

Prompts and responses are sent to the provider selected by the router. Provider
privacy and retention policies still apply. Upstream error bodies are capped
before they are surfaced, and outbound requests use connection and response
timeouts.

## Reporting a vulnerability

Do not open a public issue containing secrets or exploit details. Contact the
maintainer privately and include the affected version, reproduction steps, and
impact. Rotate any key that may have been exposed.
