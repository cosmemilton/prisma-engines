# Experimental Firebird 3 connector

These changes extend the engines baseline
`d1b3f377ca835d73fd50954a5e22128b9ebdd6b8` and require the matching modified
Prisma ORM sources based on tag `7.10.1`. Use the matching modified sources
instead of substituting another upstream revision. Use Node.js 24 or
newer for the paired JavaScript runtime and the Wasm API tests below.

The target is Firebird **3.x, SQL dialect 3, UTF8**, with a distinct `firebird`
provider. This is an experimental source integration, not an official Prisma
release or a published engines package. A checkout of the upstream baseline alone
does not contain these modifications.

## Implemented subset

- PSL validation, formatting and DMMF support; Firebird native scalar types,
  31-byte identifiers, decimal precision up to 18, and UTF8 character capacities
  up to 8191. Relations use `foreignKeys`; `NoAction`, `Cascade`, `SetNull` and
  `SetDefault` are supported, while `Restrict` is rejected.
- Firebird SQL rendering and query plans for the CRUD, filtering and pagination
  cases covered by the focused tests. Identity inserts use singleton `RETURNING`;
  `createMany` returns a count. Literal `startsWith`/`notStartsWith` use a dedicated
  comparison rather than interpreting arbitrary `LIKE` patterns.
- Updates of manually assigned and compound primary keys use the database's
  returned key for the transactional post-read. This includes key rewrites by
  `BEFORE UPDATE` triggers. Secondary or recursive key rewrites from `AFTER`
  triggers are outside the accepted contract. Bulk returning is disabled.
- Schema introspection and migration operations for the representable subset of
  persistent tables, columns, keys, indexes and defaults. Datamodel-to-datamodel
  or empty-target diffing also has an offline Wasm API, without a datasource URL.

Live operations run through **schema-engine Wasm and the paired JavaScript
Firebird adapter**, exposed by the modified ORM CLI. Native Quaint connections
and the native schema-engine CLI do not implement Firebird connectivity. Building
the native schema-engine binary does not provide a Firebird CLI.

Views, external/temporary tables, computed columns, arrays, JSON, enums,
namespaces and non-default text collations are outside this subset. Introspection
rejects unsupported catalog constructs instead of silently modeling them. Reset
rejects unmanaged objects that it cannot safely represent. Arbitrary manually
written migrations need separate review and testing.

Shadow replay requires an existing, separately provisioned database. The paired
adapter requires explicit local-filesystem opt-in, numeric loopback hosts and
distinct physical file identities before returning a shadow connection. Neither
database provisioning nor creation/deletion of database files is implemented.
Firebird 4/5 compatibility and distributed transactions, prepared-transaction
recovery or two-phase commit are not established by this implementation.

## Build on Windows with PowerShell

Install the tools separately: Rust `1.97.1-x86_64-pc-windows-msvc`, its
`wasm32-unknown-unknown` target, and `wasm-bindgen-cli` `0.2.105` matching
`Cargo.lock`. The helpers require PowerShell 5.1 or newer, validate the supplied
tools and do not install them. Run from this repository root, replacing the
three placeholders with existing installation locations:

```powershell
$firebirdTools = @{
  CargoHome = '<existing Cargo home>'
  RustupHome = '<existing Rustup home>'
  WasmBindgenPath = '<existing wasm-bindgen.exe>'
}
& .\script\build-firebird-schema.ps1 @firebirdTools
& .\script\build-firebird-query-compiler.ps1 @firebirdTools
& .\script\build-firebird-schema-engine.ps1 @firebirdTools
```

These produce `target/prisma-schema-wasm`, `target/query-compiler-wasm` and
`target/schema-engine-wasm`, respectively. All accept `-BuildProfile dev|release`
(default `dev`), `-OutputPath`, `-TargetDirectory`, `-PackageVersion` and
`-Offline`; offline mode requires dependencies already cached. Set
`RUSTUP_TOOLCHAIN=1.97.1-x86_64-pc-windows-msvc` before manual Cargo/rustup
commands to avoid the repository toolchain file requesting unrelated targets.

The query-compiler helper emits only Firebird assets. Its `fast` and `small`
names derive from the same build; they are not separately optimized variants.
The schema-engine helper enables `firebird,sqlite,postgresql`. The default `dev`
outputs are development artifacts, not production release bundles. The paired
ORM must explicitly select these local packages; a complete multi-provider ORM
build also needs matching compiler assets for its other providers. Build outputs
are ignored and are not supplied by a source checkout.

## Focused checks

With the pinned Rust environment configured, these commands select source-level
Firebird tests without connecting to a database:

```powershell
cargo test --locked -p psl --features all --test firebird --test firebird_constraints --test firebird_identifiers
cargo test --locked -p query-compiler --test firebird
cargo test --locked -p quaint --features firebird --lib firebird
cargo test --locked -p sql-schema-describer --features firebird --lib firebird
cargo test --locked -p sql-schema-connector --features firebird --lib firebird
cargo test --locked -p schema-commands --features firebird,sqlite,postgresql --lib offline_tests
```

After building the corresponding real Wasm packages at their default locations:

```powershell
node --test prisma-schema-wasm/tests/firebird-schema.test.cjs
node --test schema-engine/schema-engine-wasm/tests/firebird-offline-diff.test.mjs
```

Custom outputs can be selected with `PRISMA_SCHEMA_WASM` and
`PRISMA_SCHEMA_ENGINE_WASM_DIR`. The independent timer fixture has its own
[build and test instructions](../libs/crosstarget-utils/tests/wasm-time-harness/README.md).
These checks do not establish a full all-provider regression pass or live Client,
introspection and migration acceptance. Live checks belong in the paired ORM
and require owned synthetic databases and separately configured main/shadow
connections.

## Distribution notes

Preserve the repository [license](../LICENSE), [modification notice](../MODIFICATIONS.md)
and the original third-party licenses/notices applicable to the exact distributed
JavaScript and Wasm bytes. The build helpers are not a complete redistribution
or license-inventory workflow: only the schema-engine helper currently copies
the root license and modification notice automatically. Review package contents,
dependency attribution and version pairing before redistributing artifacts.
