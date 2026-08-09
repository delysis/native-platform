# Resource manifest

The public JSON contract is versioned as `information_native.catalog.v1`.
Unknown schema versions fail closed. Unknown optional metadata is retained only
inside an explicit extension map; it never grants capabilities.

At minimum, a release declares:

- stable resource and release identifiers;
- title, summary, languages, subjects, publisher, and homepage;
- license identifier/text URI and required attribution;
- publication time and upstream provenance;
- one or more representations with exact formats and capabilities;
- artifact byte lengths, SHA-256 digests, and one or more HTTP(S)/file mirrors;
- install scope (`full` or declared subset axes) and expected installed bytes.

Remote catalogue metadata is not itself authority to install. A catalogue may
declare a trust classification, but the runtime records a separate effective
authority: unverified digest, built-in pinned digest, or explicit local
approval. The resolved install plan repeats that authority and freezes the
exact sources, digests, rights, use policy, disk impact, and requested selection
for user or policy approval. `file:` sources additionally require a canonical
root capability at execution time.

That authority remains valid only while the executing host can bind the plan to
the catalogue that established it. Importing a plan file through the CLI is a
detached boundary: the original plan fingerprint is retained for audit, the
effective authority becomes `DetachedUnverified`, and the rewritten plan gets a
new fingerprint. A serialized built-in pin or local approval is never replayed
as current authority merely because it appears in the file.

Dynamic catalogues such as Kiwix OPDS and Overture STAC first produce bounded
discovery records. A discovery record becomes installable only after a provider
adapter resolves exact artifact length and SHA-256 metadata. Upstream
identifiers and source documents are retained in provenance so a later plan and
receipt can be audited.

Discovery and manifest expressiveness are not backend claims. This release does
not read Kiwix ZIM content and does not materialize or query OSM PBF or Overture
GeoParquet. Those adapters must produce exact derived bytes, provenance, and
receipts before their operations can be advertised.
