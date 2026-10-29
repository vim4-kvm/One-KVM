import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/virtualModifiers.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ESNext } }).outputText
const mods = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)

test('one tap arms, a quick second tap locks, a slow second tap releases', () => {
  assert.equal(mods.tapModifier(undefined, Infinity), 'armed')
  assert.equal(mods.tapModifier('armed', 200), 'locked')
  assert.equal(mods.tapModifier('armed', mods.DOUBLE_TAP_MS + 1), undefined)
  assert.equal(mods.tapModifier('locked', 100), undefined, 'tapping a locked modifier always releases it')
})

test('the next key consumes armed modifiers but keeps locked ones', () => {
  const { kept, released } = mods.consumeArmed(new Map([['ControlLeft', 'armed'], ['ShiftLeft', 'locked'], ['AltLeft', 'armed']]))
  assert.deepEqual(released, ['ControlLeft', 'AltLeft'])
  assert.deepEqual([...kept], [['ShiftLeft', 'locked']])
})

test('combo labels collapse left and right variants', () => {
  assert.equal(mods.comboLabel(['Ctrl', 'Shift', 'Ctrl']), 'Ctrl + Shift')
})
