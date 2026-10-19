/*! Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md. */
'use strict'

const assert = require('node:assert/strict')
const fs = require('node:fs')
const path = require('node:path')
const { spawnSync } = require('node:child_process')
const test = require('node:test')

const modulePath = path.resolve(
  process.env.PRISMA_WASM_TIME_HARNESS || path.join(__dirname, '../../../target/wasm-time-harness/wasm_time_harness.js'),
)
assert.ok(fs.existsSync(modulePath), `Build the real test-only Wasm timer harness first: ${modulePath}`)

// Each child must exit naturally: an uncleared 60-second timer fails the bounded
// subprocess test even if the requested Rust future returned successfully.
const childSource = String.raw`
const assert = require('node:assert/strict')
const [modulePath, mode, handleType] = process.argv.slice(1)
const nativeSetTimeout = globalThis.setTimeout
const nativeClearTimeout = globalThis.clearTimeout
const active = new Map()
const created = []
const fired = []
const cleared = []
let nextId = 1
globalThis.setTimeout = (callback, millis) => {
  let key
  const nativeHandle = nativeSetTimeout(() => {
    active.delete(key)
    fired.push(millis)
    callback()
  }, millis)
  key = handleType === 'number' ? nextId++ : nativeHandle
  created.push({ millis, type: typeof key })
  active.set(key, { nativeHandle, millis })
  return key
}
globalThis.clearTimeout = (key) => {
  const entry = active.get(key)
  if (entry) {
    cleared.push(entry.millis)
    active.delete(key)
    nativeClearTimeout(entry.nativeHandle)
  } else if (handleType === 'object') {
    nativeClearTimeout(key)
  }
}
const wasm = require(modulePath)
void (async () => {
  const started = performance.now()
  if (mode === 'cancel') {
    assert.equal(wasm.cancelAfterPending(), true, 'sleep must be polled Pending before cancellation')
    assert.equal(created.length, 1)
    assert.deepEqual(fired, [])
    assert.deepEqual(cleared, [60_000])
  } else if (mode === 'normal') {
    await wasm.normalSleep()
    assert.ok(performance.now() - started >= 15, 'sleep must actually wait for its timer')
    assert.deepEqual(fired, [25])
    assert.equal(created.length, 1)
  } else if (mode === 'work-wins') {
    assert.equal(await wasm.workWins60Sec(), true)
    assert.equal(created.length, 2, 'both futures must schedule a timer before the race completes')
    assert.deepEqual(fired, [25])
    assert.deepEqual(cleared, [60_000])
  } else if (mode === 'timeout-wins') {
    assert.equal(await wasm.timeoutWinsDrop60Sec(), true)
    assert.equal(created.length, 2)
    assert.deepEqual(fired, [25])
    assert.deepEqual(cleared, [60_000])
  } else {
    throw new Error('unknown test mode')
  }
  assert.equal(active.size, 0, 'the real Rust future must clear every pending timer')
  assert.ok(created.every((timer) => timer.type === handleType), 'preserve the actual host handle type')
  console.log(JSON.stringify({ mode, handleType, created: created.length, fired, cleared, active: active.size, elapsedMs: performance.now() - started }))
})().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
`

for (const handleType of ['object', 'number']) {
  for (const [name, mode] of [
    ['cancellation after Pending clears an actual timer', 'cancel'],
    ['normal sleep fires and measures elapsed time', 'normal'],
    ['completed work cancels the losing timeout and exits naturally', 'work-wins'],
    ['elapsed timeout cancels the losing work sleep', 'timeout-wins'],
  ]) {
    test(`${name} (${handleType} handle)`, (context) => {
      const childStarted = performance.now()
      const result = spawnSync(process.execPath, ['-e', childSource, modulePath, mode, handleType], {
        encoding: 'utf8',
        timeout: 4_500,
        windowsHide: true,
      })
      assert.ifError(result.error)
      assert.equal(result.signal, null, result.stderr)
      assert.equal(result.status, 0, result.stderr)
      const naturalExitMs = performance.now() - childStarted
      assert.ok(naturalExitMs < 5_000, 'child must exit naturally within five seconds')
      const proof = JSON.parse(result.stdout.trim())
      assert.equal(proof.active, 0)
      assert.equal(proof.mode, mode)
      assert.equal(proof.handleType, handleType)
      context.diagnostic(JSON.stringify({ ...proof, naturalExitMs }))
    })
  }
}
