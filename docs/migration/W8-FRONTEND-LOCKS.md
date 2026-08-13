# W8 frontend lock consolidation

The two accepted frontend locks were preserved by digest before consolidation:

| Input | Bytes | SHA-256 |
| --- | ---: | --- |
| root `pnpm-lock.yaml` | 4,354 | `dc21ffe6c3a2710989ebbb398e4c395f70cccaad53b30858a8a4a87b7b9ab530` |
| `products/loom/pnpm-lock.yaml` | 41,162 | `025ff786f7ff9c065b3f938df769b7d7294d9dce46e948b6bd03e6ded6952f2c` |

The consolidated root lock is 41,294 bytes with SHA-256
`1c2d7d73d3c24ab2a8d1ecce65efb30b65683dcd975212d5c62cb45273ba2714`.
It contains exactly the 134 package records and 134 snapshot records from the
union of the accepted inputs; no package resolution changed. Its importers are
the root, `products/fte`, and `products/loom/apps/loom`.

The lock was validated with pnpm 11.16.0. The consolidated workspace passed:

- Loom: 32 test files and 182 tests;
- Loom Svelte check: 0 errors and 0 warnings;
- Loom production build;
- FTE frontend: 2 tests.
