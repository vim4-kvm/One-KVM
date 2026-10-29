import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/terminalKeys.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const { consumeTerminalKey: consume, toggleTerminalModifier: toggle } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)
const idle = { ctrl: false, alt: false }

test('shell controls and cursor sequences survive the touch key row', () => {
  for (const [key, expected] of [['Esc', '\x1b'], ['Tab', '\t'], ['↑', '\x1b[A'], ['↓', '\x1b[B'], ['→', '\x1b[C'], ['←', '\x1b[D'], ['|', '|'], ['~', '~'], ['/', '/'], ['-', '-']]) {
    assert.equal(consume(idle, key).bytes, expected)
  }
})
test('sticky control is one shot, can cancel, and does not mutate prior state', () => {
  const armed = toggle(idle, 'Ctrl')
  assert.deepEqual(idle, { ctrl: false, alt: false })
  assert.equal(consume(armed, 'c').bytes, '\x03')
  assert.equal(consume(armed, 'Z').bytes, '\x1a')
  assert.equal(consume(armed, ' ').bytes, '\0')
  assert.equal(consume(armed, '[').bytes, '\x1b')
  assert.deepEqual(consume(armed, 'c').modifiers, idle)
  assert.deepEqual(toggle(armed, 'Ctrl'), idle)
})
test('Alt and combined modifiers apply to exactly one key', () => {
  const alt = toggle(idle, 'Alt')
  assert.equal(consume(alt, 'x').bytes, '\x1bx')
  assert.equal(consume(alt, '↑').bytes, '\x1b[1;3A')
  assert.equal(consume(toggle(alt, 'Ctrl'), 'c').bytes, '\x1b\x03')
  assert.equal(consume(toggle(idle, 'Ctrl'), '↑').bytes, '\x1b[1;5A')
  assert.equal(consume(toggle(alt, 'Ctrl'), '←').bytes, '\x1b[1;7D')
  assert.deepEqual(consume(alt, 'Tab').modifiers, idle)
  assert.equal(consume(consume(alt, 'x').modifiers, 'x').bytes, 'x')
})
