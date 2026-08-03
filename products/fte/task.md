# Implementation status

## Gateway core

- [x] SQLite storage for keys, settings, request logs, and restart-safe quota events
- [x] Atomic sliding-window request and token tracking
- [x] Model-aware capability gating and parameter policies
- [x] Transport-neutral backend registry with credential-independent readiness
- [x] Startup registration for inspected local model routes
- [x] OpenRouter, Groq, Mistral, NVIDIA, and Cerebras adapters
- [x] Native Anthropic and Gemini request/response/stream normalization
- [x] OpenAI-compatible models, completions, chat completions, and responses
- [x] Anthropic Messages and Gemini `generateContent` compatibility endpoints
- [x] Graceful loopback proxy restart with persisted port settings
- [x] Bounded request and upstream-error bodies, timeouts, and security headers

## Desktop application

- [x] Dashboard backed only by recorded local data
- [x] Setup for every implemented provider, including key removal
- [x] Non-secret reusable signup profile
- [x] Multi-turn chat playground with model selection
- [x] Activity log
- [x] Proxy settings
- [x] Accessible navigation, forms, status messaging, and responsive layout
- [x] Strict content security policy and scoped external-link permissions

## Quality

- [x] Rust unit and integration-style tests
- [x] Strict formatting and Clippy checks
- [x] Frontend syntax check and dependency audit
- [x] Continuous-integration workflow
- [x] Security and threat-model documentation
- [ ] Live-provider smoke tests with user-owned credentials
- [ ] Signed release packaging

## Deliberate next steps

- Ingest real evaluation results into `EvalStore`
- Add usage charts sourced from recorded request logs
- Add model-catalog refresh/versioning for fast-moving provider inventories
- Add optional OS keychain-backed credential encryption
- Add more native transports such as Ollama and AWS Bedrock
- Integrate the versioned `llama-native-host` crate after its raw-completion
  contract passes the handoff acceptance suite
