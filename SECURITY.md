# Security policy

Report vulnerabilities privately to the Delysis maintainers through GitHub's
private vulnerability-reporting channel once the repository is published. Do
not open a public issue containing credentials, private data, exploit details,
or model artifacts.

This shell contains no production runtime. Future unsafe or FFI code must stay
behind an explicitly reviewed external boundary; first-party workspace code is
safe Rust by default. Never commit secrets. Hosted-provider and operating-system
credential tests must be opt-in and must not run in portable CI.
