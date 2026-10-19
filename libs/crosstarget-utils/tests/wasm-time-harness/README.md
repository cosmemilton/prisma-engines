<!-- Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md. -->

This standalone test workspace links the real `crosstarget-utils` Wasm timers.
Its lockfile is separate from the production workspace. It was initialized from
the existing production lockfile so the cached, now-yanked `futures-util 0.3.28`
selection can be reproduced offline. Production dependencies and exports are unchanged.

From the engines repository, with the pinned Rust 1.97.1 environment configured:

```powershell
cargo build --locked --offline --manifest-path libs/crosstarget-utils/tests/wasm-time-harness/Cargo.toml --target wasm32-unknown-unknown --target-dir target
& ..\.tools\wasm-bindgen-0.2.105-x86_64-pc-windows-msvc\wasm-bindgen.exe --target nodejs --out-dir target/wasm-time-harness --out-name wasm_time_harness target/wasm32-unknown-unknown/debug/crosstarget_wasm_time_harness.wasm
node --test libs/crosstarget-utils/tests/wasm-time.test.cjs
```

The four fixture exports exercise normal sleep, cancellation after an actual
`Pending` poll, work winning against a 60-second timeout, and a timeout dropping
a 60-second work sleep. Each case runs in a separate child using both Node object
handles and numeric browser-style handles backed by real Node timers. The parent
verifies cancellation and natural exit within five seconds, without `process.exit`
or `unref` in the child.
