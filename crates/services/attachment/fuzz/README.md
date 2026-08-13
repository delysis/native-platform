# Fuzzing

The fuzz targets accept attacker-controlled bytes, logical names, and declared
media-type hints. They deliberately use small, explicit limits for root and
parser input, decoder windows, retained and derived bytes, object/edge/entry
counts, recursion depth, image pixels, transform requests, and wall time.

Build both targets without running an unbounded fuzz campaign:

```sh
cargo fuzz build inspect
cargo fuzz build pipeline
```

Run a time-bounded local campaign:

```sh
cargo fuzz run inspect -- -max_total_time=300 -max_len=65539
cargo fuzz run pipeline -- -max_total_time=300 -max_len=65539
```

- `inspect` exercises content-first detection and recursive container
  inspection. Every successful result must satisfy the full bundle contract.
- `pipeline` exercises inspection, canonicalization, and capability-aware
  planning. Successful results must satisfy both bundle and plan contracts and
  must retain authority-free receipts.

Crashes and minimized reproducers belong in `fuzz/artifacts/` and are ignored
by Git. Promote every fixed reproducer into a deterministic regression test
before deleting it locally.
