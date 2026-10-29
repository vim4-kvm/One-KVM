import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/keyboardShortcuts.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ESNext } }).outputText
const sc = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)
const known = key => /^(Key[A-Z]|Digit\d|F\d+|Control|Alt|Shift|Meta|Escape|Delete|Tab|Space|Backspace)/.test(key)

test('chord labels follow the target platform', () => {
  assert.equal(sc.chordLabel(['ShiftLeft', 'ControlLeft', 'Escape'], 'windows'), 'Ctrl+Shift+Esc')
  assert.equal(sc.chordLabel(['MetaLeft', 'KeyR'], 'windows'), 'Win+R')
  assert.equal(sc.chordLabel(['MetaLeft', 'AltLeft', 'Escape'], 'mac'), '⌥⌘Esc')
  assert.equal(sc.chordLabel(['MetaLeft'], 'mac'), '⌘')
})

test('capturing collects modifiers until a main key completes the chord', () => {
  let state = sc.captureKey([], 'ShiftLeft')
  state = sc.captureKey(state.keys, 'ControlLeft')
  assert.deepEqual(state, { keys: ['ControlLeft', 'ShiftLeft'], complete: false })
  assert.deepEqual(sc.captureKey(state.keys, 'ShiftLeft').keys, ['ControlLeft'], 'a second tap removes the modifier')
  assert.deepEqual(sc.captureKey(state.keys, 'KeyK'), { keys: ['ControlLeft', 'ShiftLeft', 'KeyK'], complete: true })
  const done = sc.captureKey(state.keys, 'KeyK').keys
  assert.deepEqual(sc.captureKey(done, 'AltLeft').keys, ['AltLeft'], 'a new chord starts after a finished one')
  assert.deepEqual(sc.captureKey(done, 'KeyT').keys, ['KeyT'])
})

test('saved shortcuts survive bad storage and drop invalid entries', () => {
  assert.deepEqual(sc.parseCustomShortcuts('not json', known), [])
  assert.deepEqual(sc.parseCustomShortcuts('{"a":1}', known), [])
  const parsed = sc.parseCustomShortcuts(JSON.stringify([
    { id: 'a', keys: ['KeyK', 'ControlLeft'], name: '  Clear  ' },
    { id: 'b', keys: ['ControlLeft', 'Bogus'] },
    { id: 'c', keys: ['KeyA', 'KeyB'] },
    { id: 'd', keys: [] },
  ]), known)
  assert.deepEqual(parsed, [{ id: 'a', keys: ['ControlLeft', 'KeyK'], name: 'Clear' }])
})

test('presets only use the documented key names', () => {
  for (const list of Object.values(sc.PRESET_SHORTCUTS)) {
    for (const shortcut of list) assert.ok(shortcut.keys.every(known), shortcut.id)
  }
  assert.ok(sc.sameChord(['KeyK', 'ControlLeft'], ['ControlLeft', 'KeyK']))
})
