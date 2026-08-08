# Mama Llama Persona Library

Mama Llama’s built-in Persona library is the exact catalog supplied in
`/Users/george/Downloads/therapy_consult_mcp/personas.yaml` on 2026-08-03.
The checked-in source is
`crates/mom-llama-runtime/assets/therapy_consult_personas.yaml`; both files have
SHA-256 `09557b34bc9c85108ab7f901ca419fa50eb60283fb79519b6eeecccf8213ea64`.

The application does not rename these Personas as abstract lenses, shorten
their system prompts, add blanket hedging, or seed substitute Personas. The
14 built-ins are:

1. Bessel van der Kolk
2. Gabor Maté
3. Peter Levine
4. Judith Herman
5. Richard Schwartz
6. Janina Fisher
7. Ad de Jongh
8. Christine Courtois
9. Robert Miller (Feeling-State Addiction Protocol)
10. Arnold Popky (DeTUR)
11. Jim Knipe
12. Francine Shapiro
13. Shirley Jean Schmidt (DNMS)
14. Dolores Mosquera

Each entry is seeded idempotently as an encrypted, versioned
`PersonaTemplate` conversation. Its visible name, stable `@handle`, modality,
and full supplied `system_prompt` come from the checked-in catalog. The main
sidebar’s **Personas** action opens the named library; selecting a row creates
and opens a normal chat sourced from that Persona. A Persona can also be
addressed from any ordinary composer using its `@handle`.

The application seeds no Consult groups. Groups are user-owned ordered
patterns of one to four Persona references and are configured in
**Settings → Consult groups**. Previous application-owned default groups are
removed during the catalog migration; user-created groups are preserved.

The migration replaces only stable application-owned seed IDs. It does not
delete or rewrite user-created Personas. Legacy custom Consult panels are
migrated into Persona templates and user-owned groups. Existing group
references to a built-in seed remain valid because the canonical IDs are
stable.

Persona-prefix cache reuse requires the exact Persona ID, version, active
source branch, execution profile, model/build/tokenizer/template/projector
fingerprint, system prompt, tool bindings, token budget, and rendered token
prefix. The host-chat suffix is never part of the stable Persona prefix. Any
mismatch falls back to ordinary native generation.
