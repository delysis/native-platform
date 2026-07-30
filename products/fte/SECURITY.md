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

## Local proxy

The API proxy binds only to IPv4 loopback (`127.0.0.1`) and does not expose
cross-origin browser access. It intentionally has no separate proxy
authentication, so any process running as the same operating-system user can
send requests through configured provider accounts. Do not run untrusted local
software under that user.

## Provider traffic

Prompts and responses are sent to the provider selected by the router. Provider
privacy and retention policies still apply. Upstream error bodies are capped
before they are surfaced, and outbound requests use connection and response
timeouts.

## Reporting a vulnerability

Do not open a public issue containing secrets or exploit details. Contact the
maintainer privately and include the affected version, reproduction steps, and
impact. Rotate any key that may have been exposed.
