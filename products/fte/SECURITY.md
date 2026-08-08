# Security

Free Token Energy is local-first, but local-first is not the same as encrypted.

## Local data

- Provider API keys are stored in the app's SQLite database. On Unix, the app
  data directory is restricted to the current user (`0700`) and the database
  file to that user (`0600`).
- Keys are not currently encrypted at rest. Use revocable, least-privilege
  provider keys and rely on full-disk encryption for protection against device
  theft or offline access.
- Request logs contain provider ID, public model ID, token count, latency,
  status, and timestamp. Prompts, responses, profile values, and API keys are
  not logged.
- The signup profile accepts only name, email, and a non-secret password hint.
  Startup removes the reusable-password field written by older builds.
- The application does not include telemetry.

## Local interfaces

The reusable `fte-loopback` server is disabled until explicitly started. It
binds only loopback addresses, validates `Host` and configured origins, applies
bounded request/stream limits, and requires a random app-private bearer token.
Hosted provider credentials never cross that interface.

The older desktop router and its IPv4-only proxy remain a migration surface.
That proxy does not have the reusable gateway's bearer-token boundary, so any
process running as the same operating-system user can send requests through
configured provider accounts while it is enabled. Do not enable that legacy
surface while running untrusted local software. Removing it after transactional
database and credential migration is the next breaking pre-1.0 step.

## Provider traffic

Prompts and responses are sent to the provider selected by the router. Provider
privacy and retention policies still apply. Upstream error bodies are capped
before they are surfaced, and outbound requests use connection and response
timeouts.

## Reporting a vulnerability

Do not open a public issue containing secrets or exploit details. Contact the
maintainer privately and include the affected version, reproduction steps, and
impact. Rotate any key that may have been exposed.
