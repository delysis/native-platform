# Security policy

Treat every submitted byte, filename, MIME declaration, archive header,
document relationship, and extracted string as attacker-controlled.

Please report vulnerabilities privately to the repository maintainers. Do not
attach sensitive source documents to a public issue. A security report should
include the affected version, a minimal synthetic reproducer when possible,
the expected budget or policy outcome, and the observed outcome.

The core library deliberately has no network or process authority. Optional
future native/FFI decoders must live behind a separately audited worker
boundary and may not silently expand the default trusted computing base.
