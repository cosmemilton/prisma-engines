# Experimental Firebird fork modifications

Modified on 2026-10-18 and 2026-10-19 from Prisma engines commit
`d1b3f377ca835d73fd50954a5e22128b9ebdd6b8`, paired with Prisma ORM 7.10.1.
The original Apache-2.0 LICENSE, Quaint LICENSE and existing copyright and
copied-code notices are preserved. This is an experimental fork.

Changes add the Firebird PSL provider/native types and validation, Quaint AST/SQL
visitor, query compiler and schema capability wiring, identity and transactional
singleton-insert createMany plans, database-returned primary keys for singleton
updates, and literal-prefix filtering with bounded UTF8 parameters. They also add
a Firebird catalog describer, introspection, schema calculation/diff/rendering,
migration history and owned-lock handling through the JavaScript adapter bridge.

Shared changes provide pure offline datamodel diffing, guarded Wasm factory
connection/disposal, cancellation of completed Wasm timeout handles, actual adapter
error propagation, and optional pool/wait metadata and owned-transaction hooks.
Focused tests, fixtures and Windows build helpers accompany these changes. Native
test helpers tolerate optional SQL-family variants without requiring Firebird in
their native-only feature set.

There is no Rust native Firebird driver or Firebird native schema-engine CLI.
Firebird uses the paired ORM JavaScript adapter and modified Wasm modules. See
[docs/firebird.md](docs/firebird.md) for build requirements, supported subset and
limitations, including unverified Firebird 4/5 and unimplemented distributed 2PC.

Historical source snapshots, exact module hashes, dependency/license inventories,
private package notices and acceptance reports remain preserved in a local archive
outside version control. This consolidated source notice does not alter those
artifacts. New binary/package redistribution requires legal payloads matching its
actual dependency closure; no package release or production approval is implied.
