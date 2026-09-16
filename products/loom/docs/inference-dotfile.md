# Optional inference servers

Loom works without configuration. Its normal local model discovery, inference,
editing, and storage paths remain in process. Power users can opt into inference
servers with **`~/.loom.toml`**. There are no new settings panels or server controls.
Restart Loom after changing the file. A missing or invalid file leaves the native
path available; invalid files produce one diagnostic in the application log,
with a line and column for syntax errors and without echoing source or credentials.

This is the same TOML syntax used inside the workspace `.loom.md` file's
`loom-workspace` block: named tables, snake_case keys, quoted strings, arrays, and
`#` comments. The separate home-directory file grants server and credential
authority; opening someone else's workspace must not grant that authority.
The feature is included in every desktop build and needs no feature flag,
plugin installation, setup screen, button, or menu.

For a single local server, create `~/.loom.toml` with:

```toml
version = 1

[inference]
suggestions = ["desk"]

[inference.servers.desk]
endpoint = "http://127.0.0.1:8080/v1/completions"
model = "my-writer"
context_tokens = 8192
auth = "none"
```

Change the endpoint, model, and context size to match your server. Restart Loom;
its ordinary suggestions now use that route. Add `weave = ["desk"]` under
`[inference]` to use it for manual weaving too. Comment out either scope or set
it to `[]` to restore native inference for that scope. Server definitions alone
activate nothing. Removing all inference settings, leaving only `version = 1`,
is also valid. Timeout and quota settings are optional.

For ordered fallback and a credentialed server:

```toml
version = 1

[inference]
# Exact order: the first eligible server is tried first.
suggestions = ["desk", "spare"]
# Omit a scope, or use [], to retain native inference for that scope.
weave = ["desk"]

[inference.servers.desk]
endpoint = "http://127.0.0.1:8080/v1/completions"
model = "my-writer"
context_tokens = 8192
auth = "none"
timeout_seconds = 60

[inference.servers.spare]
endpoint = "https://inference.example.com/v1/completions"
model = "my-writer"
context_tokens = 8192
auth = "bearer"
credential = "spare-writer"
timeout_seconds = 60

# Optional finite local admission limits; omission means unknown/unmetered.
[inference.servers.spare.quota]
requests_per_minute = 10
requests_per_day = 500
```

The example addresses and model names are placeholders. `endpoint` is the complete
OpenAI-compatible **raw text completion** URL. Loom does not convert continuation
prompts into chat messages. The service must implement `/completions` semantics
and return one text choice. Text attachment context is included. Image and audio
context require the native multimodal path; a configured text route rejects those
requests before sending anything.

`suggestions` covers ghost text and Loompad; the existing project automation
switch and Focus mode still govern them. `weave` covers explicitly requested
manual weaving. Each family supports up to four branches. An empty scope does not
authorize network use. A nonempty scope authorizes sending its exact writing
prompt and selected text context to the named servers. Project folders cannot
authorize servers or override this file. The workspace `.loom.md` model selection
continues to govern native inference in unconfigured scopes, chat panes, and the
terminal. A configured suggestions scope does not require loading that local model.

Routes are tried in their listed order after capability, credential, quota, and
circuit checks. Fallback applies to eligible transient setup failures before a
provider returns a generation ticket. Authentication and invalid-request errors
are not blindly retried. Once a server accepts a request, a failed response body
does not cause another server to repeat it. Completed branches remain on their
actual routes; output from different attempts is never spliced together. The
server path currently publishes each branch after its complete response is stored.

`auth = "none"` explicitly selects an unauthenticated server. For bearer auth,
`credential` names an OS keychain entry with service **`org.loom.inference`** and
that account name. Add the entry with your operating system's credential manager.
Put the token in the entry's password field, never in this file or a URL. Credential
replacement and deletion affect the next operation without restarting Loom.
Already admitted requests can finish with their captured credential. Redirects
are disabled, including redirects between endpoints on the same host.

Limits are process-local minute/day windows shared by the configured route across
scopes. They reset on restart and are not an account-wide billing guarantee.
Optional `tokens_per_minute` and `tokens_per_day` account for exact provider usage
and reserve output allowance. Input tokenization remains server-owned; unknown
usage conservatively exhausts a finite token window until it resets. Provider
limits remain authoritative. Configuring zero denies that route for that limit.

The file is limited to 64 KiB, sixteen servers, and sixteen distinct routes per
scope. Unknown fields are errors. Timeouts range from 1 to 600 seconds, with a
120-second total generation-request deadline including routing and queuing.
Configuration is read once per launch, so editing it does not silently change
the destination of work in progress.

Loom preserves the exact prompt, configuration identity, selected route, sampling
request, normalized text response, and reported usage in its normal project provenance store.
These records use `server_response` evidence. Model weights and tokenizer
fingerprints are unknown; token IDs and native cache receipts are not invented.
The normal document identity checks, immutable records, cancellation, replay, and
explicit promotion boundary remain in force. No FTE dashboard, gateway database,
or loopback listener is started by Loom.

For isolated native acceptance, the existing `DELYSIS_LOOM_ACCEPTANCE_DIR`
mode reads `.loom.toml` from its verified temporary application-data directory
and does not read the user's global configuration. The
`inference_acceptance_server` example is an explicitly synthetic local fixture.
