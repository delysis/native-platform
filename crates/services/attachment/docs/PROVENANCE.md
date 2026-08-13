# Provenance contract

Every object records:

- SHA-256 and actual byte length;
- all derivation parents;
- inert raw/logical member name;
- root or archive depth;
- detected and declared format evidence;
- transform and implementation version;
- complete, partial, blocked, malformed, or unsupported status;
- truncation and budget reasons.

Every canonical artifact records its source object, processor identity/version,
effective policy hash, content offsets in UTF-8 bytes, and warnings. Cache keys
must include the source hash, processor version, effective policy, and target
capability fingerprint.
