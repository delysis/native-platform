# ADR-007: One modern Gateway

## Status

Accepted — 2026-08-12. Steward review is recorded in `../W1-ADRS-RECEIPT.json`.

## Context and current evidence

FTE historically contained a desktop Router alongside the reusable modern Gateway. That duplicated route authority, provider models, protocol behavior, credential handling, response storage, and shutdown ownership. Phase one established the modern Gateway as the credible shared runtime used by local and hosted adapters, loopback, plugins, and Mom integration, with fixture-backed hosted-provider contracts, a real local Qwen path, OS-backed credential migration/readback evidence, and joined shutdown. No live paid-provider execution is claimed or required for phase-one promotion.

There are no existing users requiring preservation of the legacy Router API. Persisted provider metadata, usage, response history, and secret migration safety remain recovery obligations.

## Decision

The modern reusable Gateway is the sole text/model route authority for FTE desktop, Mom, plugins, and optional loopback. Each application composition root constructs one `GatewayOwner`; consumers receive handles. The Gateway owns its admitted operations and drains them on shutdown, but a native adapter borrows `NativeLlamaHandle` and may never final-shut `NativeLlamaOwner`.

FTE command families migrate to the modern Gateway and then their legacy Router paths are deleted. Hosted providers live behind provider adapters and injected secret-resolution interfaces. Protocol edges preserve protocol-native semantics instead of flattening requests into one lowest-common-denominator DTO. Optional speech protocol edges delegate to `SpeechHandle`; the text Gateway does not own Speech.

## Alternatives

- Retain Router and Gateway indefinitely: rejected because two writable route authorities cannot be made reliably equivalent.
- Put routing in each product: rejected because it recreates provider, lifecycle, and policy drift.
- Make every provider conform to one lossy request type: rejected because it hides capability and protocol differences.
- Require a live paid-provider call for contract acceptance: rejected; strict fixtures prove adapter behavior, while any live receipt must be named separately and narrowly.

## Migration

1. Inventory every legacy FTE command, data path, UI consumer, store, credential, and modern equivalent.
2. Move provider inventory/readiness, model catalog, nonsecret configuration, credentials, playground chat, raw completions, Responses, usage, loopback, and settings in bounded families.
3. For each family, compare normalized legacy and modern fixtures, document intentional differences, switch authority once, and delete the legacy call path.
4. Migrate secrets to the OS store with exact write/readback before deleting legacy rows; retain only needed nonsecret metadata.
5. Delete the historical Router, duplicate models/proxy/registry, and plaintext secret schema after all command families have one authority.

## Rollback

Before a family cutover, rollback is a code revert. After cutover, restore the last known-good modern Gateway path and migrate data from versioned backups; do not reactivate Router and Gateway as writable peers. Credential rollback must never delete the OS-store value until exact recovery or replacement is verified.

## Acceptance

- Every FTE desktop, plugin, loopback, and Mom route names one modern Gateway implementation.
- Architecture tests find no production legacy Router authority or duplicate writable provider store.
- Local Qwen, strict hosted fixtures, loopback, cancellation, and joined shutdown gates pass.
- Secret migration proves exact OS-store readback before legacy deletion, without recording secret bytes.
- Unsupported provider features fail as typed capability errors rather than silent degradation.

## Consequences

Routing policy, lifecycle, and provider fixes land once. Product composition roots remain responsible for construction and final process order. Provider-specific adapters and DTO conversion remain necessary, but duplicate routing authority does not.
