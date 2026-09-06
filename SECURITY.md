# Security policy

Report vulnerabilities privately to the Delysis maintainers through GitHub's
private vulnerability-reporting channel once the repository is published. Do
not open a public issue containing credentials, private data, exploit details,
or model artifacts.

The workspace contains native inference, hosted-provider, desktop, and sibling
service runtimes. First-party Rust forbids unsafe code; native execution uses
reviewed external FFI-bearing dependencies outside that boundary. Never commit secrets. Hosted-provider and operating-system
credential tests must be opt-in and must not run in portable CI.
