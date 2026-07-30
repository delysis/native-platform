# Provider Gateway Research Notes

This directory contains cloned references for building a Rust, LiteLLM-like
provider interface. The goal is not to vendor these projects; it is to mine the
bug-compatible details that real multi-provider clients already had to learn.

## Cloned references

Best direct references:

- `litellm` - https://github.com/BerriAI/litellm
- `liter-llm` - https://github.com/kreuzberg-dev/liter-llm
- `rust-genai` - https://github.com/jeremychone/rust-genai
- `llm-connector` - https://github.com/lipish/llm-connector
- `anyllm-proxy` - https://github.com/whit3rabbit/anyllm-proxy
- `edgequake-llm` - https://github.com/raphaelmansuy/edgequake-llm
- `llmg` - https://github.com/modpotatodotdev/llmg

Useful secondary references:

- `litellm-rs` - https://github.com/majiayu000/litellm-rs
- `litellm-rust` - https://github.com/avivsinai/litellm-rust
- `ai_api_provider` - https://github.com/weykon/ai_api_provider
- `agentix` - https://github.com/ozongzi/agentix
- `ultrafast-ai-gateway` - https://github.com/techgopal/ultrafast-ai-gateway
- `inference-gateway` - https://github.com/inference-gateway/inference-gateway
- `hoosh` - https://github.com/MacCracken/hoosh
- `inferxgate` - https://github.com/jasmedia/inferxgate
- `routerly` - https://github.com/Inebrio/Routerly
- `sentinel` - https://github.com/fbk2111/Sentinel
- `traceloop-hub` - https://github.com/traceloop/hub
- `multi-llm` - https://github.com/darval/multi-llm
- `limit` - https://github.com/marioidival/limit

The `llm-gateway` crate advertises a repository at
`https://github.com/xinference/llm-gateway`, but that repository was not
cloneable when checked.

## Main architectural lesson

A durable provider abstraction should not stop at a single `Provider::chat`.
These projects split, explicitly or implicitly, across:

- auth scheme: bearer, `x-api-key`, Azure `api-key`, Google API key, OAuth,
  AWS SigV4, local unauthenticated
- URL shape: OpenAI path, Azure deployment path, Gemini method URL, Anthropic
  `/messages`, Bedrock runtime paths, Ollama local endpoints
- request transform: role conversion, parameter rename/drop/default, tools,
  JSON schema cleanup, multimodal content, cached content
- response transform: content, tool calls, reasoning/thinking, usage, provider
  metadata, finish reasons, error bodies
- stream parser: OpenAI SSE, Anthropic typed SSE, Gemini SSE without `[DONE]`,
  Bedrock eventstream, Ollama NDJSON, provider-specific pseudo-streaming
- model policy: capabilities and supported parameters are model-specific, not
  just provider-specific

Free Token Energy now implements this split through `ProviderSpec`, the model
catalog, request/response transforms, model parameter policies, and separate
OpenAI, Anthropic, and Gemini stream parsers.

## Streaming lessons

The local proxy now handles OpenAI-compatible SSE, but the broader target needs
a stream-normalization layer:

- Auto-detect or declare stream format: SSE, NDJSON, AWS EventStream, or native.
- Preserve OpenAI chunk shape at the public boundary.
- Accumulate tool call deltas by `index`; many providers stream partial
  function argument JSON.
- Normalize reasoning fields from `reasoning`, `thinking`, or `<think>` tags
  into an internal reasoning channel before deciding what to expose.
- Treat usage as late-arriving state. Some providers only send usage in the
  final chunk, some in a provider metadata object, and some not at all.
- Do not assume `[DONE]`; Gemini native streams terminate by HTTP EOF or
  finish reason.

`llm-connector` is especially useful here because it models stream formats and
parse modes separately from provider identity.

## Provider quirks to capture

### OpenRouter

- OpenAI-compatible chat endpoint, but it has extra request fields:
  `transforms`, `models`, `route`, `reasoning_effort`, and `thinking`.
- LiteLLM asks for usage/cost data with a provider-specific usage include flag.
- Streaming deltas may include `reasoning`; normalize it separately from
  visible assistant text.
- Streaming chunks can carry provider errors; do not assume every `data:` frame
  is a normal chat chunk.
- Prompt caching is model-family sensitive. Cache control belongs inside
  content blocks, and Anthropic-compatible models have a small cache-breakpoint
  limit.

### Groq

- OpenAI-compatible at `https://api.groq.com/openai/v1`.
- Strip null `function_call` fields from assistant messages.
- Some reasoning models support `reasoning_effort`.
- Structured output plus streaming may need special handling on models without
  native response schema support.
- Usage may appear in provider-specific metadata; `rust-genai` handles a Groq
  `x_groq.usage` path.

### Mistral

- OpenAI-compatible enough for simple chat, but parameter policy is uneven.
- `tool_choice: "required"` maps to Mistral's `"any"`.
- `max_completion_tokens` maps to `max_tokens`; `seed` maps to `random_seed`.
- Some implementations strip unsupported OpenAI fields such as `logit_bias`,
  penalties, and `parallel_tool_calls`; support varies by model/API version.
- Remove empty assistant messages and most message `name` fields.
- Clean tool JSON schemas aggressively: remove `$id`, `$schema`, refs, `strict`,
  and fields Mistral rejects.
- Older paths convert text-only content arrays back to a string unless the
  message has image or file content.

### NVIDIA NIM

- OpenAI-compatible at `https://integrate.api.nvidia.com/v1`.
- Parameter support is model-dependent.
- Map `max_completion_tokens` to `max_tokens`.
- Some models only safely support stream, temperature, top_p, max_tokens, stop,
  and seed; tools and response_format should be capability-gated.

### Cerebras

- OpenAI-compatible at `https://api.cerebras.ai/v1`.
- Supports the common chat parameters, with `max_completion_tokens` mapped to
  `max_tokens`.
- Reasoning controls are model-specific; do not send `reasoning_effort` unless
  the model catalog marks support.

### Anthropic

- Native endpoint is `/messages`, not `/chat/completions`.
- Auth is `x-api-key` plus required `anthropic-version`.
- Optional beta headers are feature-driven: thinking, prompt caching, PDFs,
  computer use, web search, code execution, and similar tools.
- Requires `max_tokens`; several clients choose a default if absent.
- System/developer messages become top-level `system` blocks.
- Tool results become user content blocks of type `tool_result`.
- Consecutive same-role messages need merging.
- `stop` maps to `stop_sequences`.
- Streaming is typed SSE: `message_start`, `content_block_start`,
  `content_block_delta`, `message_delta`, `message_stop`, and pings. A parser
  must track active text, tool, and thinking blocks.

### Gemini / Google AI

- Native Gemini is not OpenAI chat. It uses
  `models/{model}:generateContent`; streaming uses SSE with `alt=sse`.
- Auth can be `x-goog-api-key` for Google AI Studio, or OAuth bearer for
  Vertex.
- OpenAI messages become `contents`; system/developer becomes
  `systemInstruction`; assistant role becomes `model`.
- OpenAI tool calls map to `functionCall`; tool results map to
  `functionResponse`.
- `response_format` maps into `generationConfig.responseMimeType`.
- `tool_choice` maps into `toolConfig.functionCallingConfig.mode`.
- Native streams usually do not emit `[DONE]`; usage can appear on chunks, and
  terminal state is inferred from finish reasons or EOF.

### Azure OpenAI

- Auth header is `api-key`, not bearer.
- The deployment name is in the URL:
  `/openai/deployments/{deployment}/chat/completions?api-version=...`.
- Remove `model` from the body after it is represented as the deployment.
- O-series models need stricter parameter filtering; some remove temperature,
  top_p, stream, and stream_options depending on model/version.
- Content filter responses can omit the usual assistant message; normalize to a
  minimal OpenAI-compatible choice instead of crashing.

### Bedrock

- Requires AWS SigV4 signing.
- Streaming may use AWS EventStream, not SSE.
- The normalizer needs a separate Bedrock transport before it can share the
  OpenAI public response surface.

### Ollama and local runtimes

- Commonly stream NDJSON instead of SSE.
- Final usage is provider-specific, e.g. prompt/eval counts and done reasons.
- Local providers often have no API key and should be modeled separately from
  cloud providers rather than as empty-key OpenAI clones.

## Free Token Energy implications

Implemented:

1. OpenAI-compatible adapters for OpenRouter, Groq, Mistral, NVIDIA, and
   Cerebras.
2. Catalog-backed auth, endpoint, parameter-policy, quota, and model-capability
   metadata.
3. Native Anthropic Messages and Gemini `generateContent` transforms.
4. Stateful OpenAI, Anthropic, and Gemini SSE normalization with fixture-style
   tests.

Remaining:

1. Add Ollama NDJSON and Bedrock EventStream transports.
2. Store carefully bounded provider metadata needed for cache and cost
   analysis without logging prompts, responses, or secrets.
3. Expand golden fixtures for tool-call and multimodal edge cases.
