<script setup lang="ts">
import { useConsoleAppearance } from "@/composables/useConsoleAppearance"
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import Keyboard from 'simple-keyboard'
import 'simple-keyboard/build/css/index.css'
import { hidApi } from '@/api'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Ellipsis, Keyboard as KeyboardIcon, PanelBottom, PictureInPicture2, Play, Plus, SkipBack, SkipForward, Square, Trash2, Volume1, Volume2, VolumeX, X } from 'lucide-vue-next'
import { CanonicalKey } from '@/types/generated'
import {
  keys,
  consumerKeys,
  latchingKeys,
  modifiers,
  updateModifierMaskForKey,
  type KeyName,
  type ConsumerKeyName,
  normalizeKeyboardCode,
} from '@/lib/keyboardMappings'
import {
  MAX_CUSTOM_SHORTCUTS,
  PRESET_SHORTCUTS,
  STORAGE_KEY as SHORTCUTS_STORAGE_KEY,
  captureKey,
  chordLabel,
  isModifierKey,
  parseCustomShortcuts,
  sameChord,
  type Shortcut,
} from '@/lib/keyboardShortcuts'
import {
  type KeyboardOsType,
  osBottomRows,
  mediaKeys,
} from '@/lib/keyboardLayouts'
import { comboLabel, consumeArmed, tapModifier, type ModifierLatch } from '@/lib/virtualModifiers'

const props = defineProps<{
  visible: boolean
  attached?: boolean
  capsLock?: boolean
  pressedKeys?: CanonicalKey[]
  consumerEnabled?: boolean
}>()

const emit = defineEmits<{
  (e: 'update:visible', value: boolean): void
  (e: 'update:attached', value: boolean): void
  (e: 'keyDown', key: CanonicalKey): void
  (e: 'keyUp', key: CanonicalKey): void
}>()

const { t } = useI18n()
const consoleAppearance = useConsoleAppearance()

const isAttached = ref(props.attached ?? true)
const selectedOs = ref<KeyboardOsType>('windows')

const mainKeyboard = ref<Keyboard | null>(null)

const pressedModifiers = ref<number>(0)
const keysDown = ref<CanonicalKey[]>([])
/** Modifiers held by the on-screen keyboard; the target sees them as pressed. */
const modifierLatches = ref(new Map<CanonicalKey, ModifierLatch>())
const lastModifierTap = new Map<CanonicalKey, number>()
/** Keys kept highlighted briefly after a click so the press is visible. */
const flashedKeys = ref<string[]>([])
const runningMacro = ref<string | null>(null)
const FLASH_MS = 140

const isShiftActive = computed(() => {
  return (pressedModifiers.value & 0x22) !== 0
})

const areLettersUppercase = computed(() => {
  return Boolean(props.capsLock) !== isShiftActive.value
})

const layoutName = computed(() => {
  return isShiftActive.value ? 'shift' : 'default'
})

const keyNamesForDownKeys = computed(() => {
  // Latched modifiers have their own armed/locked styling.
  return Array.from(new Set([
    ...(props.pressedKeys ?? []),
    ...keysDown.value,
    ...flashedKeys.value,
    ...(props.capsLock ? ['CapsLock'] : []),
  ])).filter(key => !modifierLatches.value.has(key as CanonicalKey))
})

const latchedKeyNames = (latch: ModifierLatch) => {
  if (capture.value) return latch === 'armed' ? capture.value.keys.filter(isModifierKey).join(' ') : ''
  return [...modifierLatches.value].filter(([, value]) => value === latch).map(([key]) => key).join(' ')
}

const comboText = computed(() => comboLabel([...modifierLatches.value.keys()].map(key => keyDisplayMap.value[key] ?? key)))

const keyboardRef = ref<HTMLDivElement | null>(null)
const isDragging = ref(false)
const dragOffset = ref({ x: 0, y: 0 })
const position = ref({ x: 100, y: 100 })

const keyboardId = ref(`kb-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`)

const getBottomRow = () =>
  [...osBottomRows[selectedOs.value], 'ArrowLeft', 'ArrowDown', 'ArrowRight'].join(' ')

const keyboardLayout = {
  main: {
    default: [
      'Escape F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 PrintScreen ScrollLock Pause',
      'Backquote Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8 Digit9 Digit0 Minus Equal Backspace Insert Home PageUp',
      'Tab KeyQ KeyW KeyE KeyR KeyT KeyY KeyU KeyI KeyO KeyP BracketLeft BracketRight Backslash Delete End PageDown',
      'CapsLock KeyA KeyS KeyD KeyF KeyG KeyH KeyJ KeyK KeyL Semicolon Quote Enter',
      'ShiftLeft KeyZ KeyX KeyC KeyV KeyB KeyN KeyM Comma Period Slash ShiftRight ArrowUp',
      'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
    ],
    shift: [
      'Escape F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 PrintScreen ScrollLock Pause',
      '(Backquote) (Digit1) (Digit2) (Digit3) (Digit4) (Digit5) (Digit6) (Digit7) (Digit8) (Digit9) (Digit0) (Minus) (Equal) Backspace Insert Home PageUp',
      'Tab (KeyQ) (KeyW) (KeyE) (KeyR) (KeyT) (KeyY) (KeyU) (KeyI) (KeyO) (KeyP) (BracketLeft) (BracketRight) (Backslash) Delete End PageDown',
      'CapsLock (KeyA) (KeyS) (KeyD) (KeyF) (KeyG) (KeyH) (KeyJ) (KeyK) (KeyL) (Semicolon) (Quote) Enter',
      'ShiftLeft (KeyZ) (KeyX) (KeyC) (KeyV) (KeyB) (KeyN) (KeyM) (Comma) (Period) (Slash) ShiftRight ArrowUp',
      'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
    ],
  },
}

const compactMainLayout = {
  default: [
    'Escape Insert Delete Home End PageUp PageDown PrintScreen',
    'Backquote Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8 Digit9 Digit0 Minus Equal Backspace',
    'Tab KeyQ KeyW KeyE KeyR KeyT KeyY KeyU KeyI KeyO KeyP BracketLeft BracketRight Backslash',
    'CapsLock KeyA KeyS KeyD KeyF KeyG KeyH KeyJ KeyK KeyL Semicolon Quote Enter',
    'ShiftLeft KeyZ KeyX KeyC KeyV KeyB KeyN KeyM Comma Period Slash ShiftRight ArrowUp',
    'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
  ],
  shift: [
    'Escape Insert Delete Home End PageUp PageDown PrintScreen',
    '(Backquote) (Digit1) (Digit2) (Digit3) (Digit4) (Digit5) (Digit6) (Digit7) (Digit8) (Digit9) (Digit0) (Minus) (Equal) Backspace',
    'Tab (KeyQ) (KeyW) (KeyE) (KeyR) (KeyT) (KeyY) (KeyU) (KeyI) (KeyO) (KeyP) (BracketLeft) (BracketRight) (Backslash)',
    'CapsLock (KeyA) (KeyS) (KeyD) (KeyF) (KeyG) (KeyH) (KeyJ) (KeyK) (KeyL) (Semicolon) (Quote) Enter',
    'ShiftLeft (KeyZ) (KeyX) (KeyC) (KeyV) (KeyB) (KeyN) (KeyM) (Comma) (Period) (Slash) ShiftRight ArrowUp',
    'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
  ],
}

const isCompactLayout = ref(false)
let compactLayoutMedia: MediaQueryList | null = null
let compactLayoutListener: ((event: MediaQueryListEvent) => void) | null = null

function setCompactLayout(active: boolean) {
  if (isCompactLayout.value === active) return
  isCompactLayout.value = active
  updateKeyboardLayout()
}

const keyDisplayMap = computed<Record<string, string>>(() => {
  const compact = isCompactLayout.value
  const metaLabel = selectedOs.value === 'windows' ? (compact ? '⊞' : '⊞ Win')
    : selectedOs.value === 'mac' ? (compact ? '⌘' : '⌘ Cmd') : 'Meta'

  return {
    CtrlAltDelete: 'Ctrl+Alt+Del',
    AltMetaEscape: 'Alt+Meta+Esc',
    CtrlAltBackspace: 'Ctrl+Alt+Bksp',

    ControlLeft: 'Ctrl',
    ControlRight: 'Ctrl',
    ShiftLeft: 'Shift',
    ShiftRight: 'Shift',
    AltLeft: 'Alt',
    AltRight: compact ? 'Alt' : 'AltGr',
    MetaLeft: metaLabel,
    MetaRight: metaLabel,
    ContextMenu: compact ? '☰' : 'Menu',

    Escape: 'Esc',
    Backspace: '⌫',
    Tab: 'Tab',
    CapsLock: 'Caps',
    Enter: 'Enter',
    Space: ' ',

    Insert: 'Ins',
    Delete: 'Del',
    Home: 'Home',
    End: 'End',
    PageUp: 'PgUp',
    PageDown: 'PgDn',

    ArrowUp: '↑',
    ArrowDown: '↓',
    ArrowLeft: '←',
    ArrowRight: '→',

    PrintScreen: 'PrtSc',
    ScrollLock: 'ScrLk',
    Pause: 'Pause',

    F1: 'F1', F2: 'F2', F3: 'F3', F4: 'F4',
    F5: 'F5', F6: 'F6', F7: 'F7', F8: 'F8',
    F9: 'F9', F10: 'F10', F11: 'F11', F12: 'F12',

    KeyA: areLettersUppercase.value ? 'A' : 'a',
    KeyB: areLettersUppercase.value ? 'B' : 'b',
    KeyC: areLettersUppercase.value ? 'C' : 'c',
    KeyD: areLettersUppercase.value ? 'D' : 'd',
    KeyE: areLettersUppercase.value ? 'E' : 'e',
    KeyF: areLettersUppercase.value ? 'F' : 'f',
    KeyG: areLettersUppercase.value ? 'G' : 'g',
    KeyH: areLettersUppercase.value ? 'H' : 'h',
    KeyI: areLettersUppercase.value ? 'I' : 'i',
    KeyJ: areLettersUppercase.value ? 'J' : 'j',
    KeyK: areLettersUppercase.value ? 'K' : 'k',
    KeyL: areLettersUppercase.value ? 'L' : 'l',
    KeyM: areLettersUppercase.value ? 'M' : 'm',
    KeyN: areLettersUppercase.value ? 'N' : 'n',
    KeyO: areLettersUppercase.value ? 'O' : 'o',
    KeyP: areLettersUppercase.value ? 'P' : 'p',
    KeyQ: areLettersUppercase.value ? 'Q' : 'q',
    KeyR: areLettersUppercase.value ? 'R' : 'r',
    KeyS: areLettersUppercase.value ? 'S' : 's',
    KeyT: areLettersUppercase.value ? 'T' : 't',
    KeyU: areLettersUppercase.value ? 'U' : 'u',
    KeyV: areLettersUppercase.value ? 'V' : 'v',
    KeyW: areLettersUppercase.value ? 'W' : 'w',
    KeyX: areLettersUppercase.value ? 'X' : 'x',
    KeyY: areLettersUppercase.value ? 'Y' : 'y',
    KeyZ: areLettersUppercase.value ? 'Z' : 'z',

    '(KeyA)': areLettersUppercase.value ? 'A' : 'a',
    '(KeyB)': areLettersUppercase.value ? 'B' : 'b',
    '(KeyC)': areLettersUppercase.value ? 'C' : 'c',
    '(KeyD)': areLettersUppercase.value ? 'D' : 'd',
    '(KeyE)': areLettersUppercase.value ? 'E' : 'e',
    '(KeyF)': areLettersUppercase.value ? 'F' : 'f',
    '(KeyG)': areLettersUppercase.value ? 'G' : 'g',
    '(KeyH)': areLettersUppercase.value ? 'H' : 'h',
    '(KeyI)': areLettersUppercase.value ? 'I' : 'i',
    '(KeyJ)': areLettersUppercase.value ? 'J' : 'j',
    '(KeyK)': areLettersUppercase.value ? 'K' : 'k',
    '(KeyL)': areLettersUppercase.value ? 'L' : 'l',
    '(KeyM)': areLettersUppercase.value ? 'M' : 'm',
    '(KeyN)': areLettersUppercase.value ? 'N' : 'n',
    '(KeyO)': areLettersUppercase.value ? 'O' : 'o',
    '(KeyP)': areLettersUppercase.value ? 'P' : 'p',
    '(KeyQ)': areLettersUppercase.value ? 'Q' : 'q',
    '(KeyR)': areLettersUppercase.value ? 'R' : 'r',
    '(KeyS)': areLettersUppercase.value ? 'S' : 's',
    '(KeyT)': areLettersUppercase.value ? 'T' : 't',
    '(KeyU)': areLettersUppercase.value ? 'U' : 'u',
    '(KeyV)': areLettersUppercase.value ? 'V' : 'v',
    '(KeyW)': areLettersUppercase.value ? 'W' : 'w',
    '(KeyX)': areLettersUppercase.value ? 'X' : 'x',
    '(KeyY)': areLettersUppercase.value ? 'Y' : 'y',
    '(KeyZ)': areLettersUppercase.value ? 'Z' : 'z',

    Digit1: '1', Digit2: '2', Digit3: '3', Digit4: '4', Digit5: '5',
    Digit6: '6', Digit7: '7', Digit8: '8', Digit9: '9', Digit0: '0',

    '(Digit1)': '!', '(Digit2)': '@', '(Digit3)': '#', '(Digit4)': '$', '(Digit5)': '%',
    '(Digit6)': '^', '(Digit7)': '&', '(Digit8)': '*', '(Digit9)': '(', '(Digit0)': ')',

    Minus: '-', '(Minus)': '_',
    Equal: '=', '(Equal)': '+',
    BracketLeft: '[', '(BracketLeft)': '{',
    BracketRight: ']', '(BracketRight)': '}',
    Backslash: '\\', '(Backslash)': '|',
    Semicolon: ';', '(Semicolon)': ':',
    Quote: "'", '(Quote)': '"',
    Comma: ',', '(Comma)': '<',
    Period: '.', '(Period)': '>',
    Slash: '/', '(Slash)': '?',
    Backquote: '`', '(Backquote)': '~',
  }
})

const customShortcuts = ref<Shortcut[]>(parseCustomShortcuts(localStorage.getItem(SHORTCUTS_STORAGE_KEY), key => key in keys))
const shortcuts = computed(() => [...PRESET_SHORTCUTS[selectedOs.value], ...customShortcuts.value])
const shortcutText = (shortcut: Shortcut) => shortcut.name || chordLabel(shortcut.keys, selectedOs.value)

/** A chord being recorded for a new custom shortcut; keys go nowhere while it is set. */
const capture = ref<{ keys: string[]; name: string } | null>(null)
const captureLabel = computed(() => capture.value?.keys.length ? chordLabel(capture.value.keys, selectedOs.value) : '')
const captureDuplicate = computed(() => !!capture.value?.keys.length && shortcuts.value.some(s => sameChord(s.keys, capture.value!.keys)))

function saveCustomShortcuts(list: Shortcut[]) {
  customShortcuts.value = list
  localStorage.setItem(SHORTCUTS_STORAGE_KEY, JSON.stringify(list))
}

async function startCapture() {
  if (customShortcuts.value.length >= MAX_CUSTOM_SHORTCUTS) return
  await releaseAllModifiers()
  capture.value = { keys: [], name: '' }
  window.addEventListener('keydown', onCaptureKey, true)
  window.addEventListener('keyup', onCaptureKey, true)
}

function stopCapture() {
  capture.value = null
  window.removeEventListener('keydown', onCaptureKey, true)
  window.removeEventListener('keyup', onCaptureKey, true)
}

function commitCapture() {
  const draft = capture.value
  if (!draft?.keys.length || captureDuplicate.value) return
  const name = draft.name.trim().slice(0, 24)
  saveCustomShortcuts([...customShortcuts.value, { id: `custom:${Date.now().toString(36)}`, keys: draft.keys, ...(name ? { name } : {}) }])
  stopCapture()
}

function deleteShortcut(id: string) {
  saveCustomShortcuts(customShortcuts.value.filter(s => s.id !== id))
}

function captureInto(key: string) {
  if (!capture.value || !(key in keys)) return
  capture.value = { ...capture.value, keys: captureKey(capture.value.keys, key).keys }
}

/** Physical keys while recording are captured here and never reach the target. */
function onCaptureKey(event: KeyboardEvent) {
  const target = event.target as HTMLElement | null
  event.stopImmediatePropagation()
  if (target?.closest('[data-shortcut-name]')) {
    if (event.type === 'keydown' && event.key === 'Enter') { event.preventDefault(); commitCapture() }
    return
  }
  event.preventDefault()
  if (event.type !== 'keydown' || event.repeat) return
  const code = normalizeKeyboardCode(event.code, event.key)
  // A bare physical modifier press is ignored until the chord's main key arrives.
  if (isModifierKey(code)) return
  if (!capture.value) return
  const held = [
    event.ctrlKey && 'ControlLeft', event.altKey && 'AltLeft', event.shiftKey && 'ShiftLeft', event.metaKey && 'MetaLeft',
  ].filter((key): key is string => !!key)
  // Modifiers picked on the on-screen keyboard combine with the physical ones.
  if (code in keys) capture.value = { ...capture.value, keys: captureKey([...(capture.value.keys.some(k => !isModifierKey(k)) ? [] : capture.value.keys), ...held], code).keys }
}

async function runShortcut(shortcut: Shortcut) {
  if (runningMacro.value) return
  runningMacro.value = shortcut.id
  const started = Date.now()
  try {
    await executeMacro([{ keys: shortcut.keys.filter(key => !isModifierKey(key)), modifiers: shortcut.keys.filter(isModifierKey) }])
    await consumeArmedModifiers()
  } finally {
    // Keep the pressed state long enough to be seen even when the shortcut is instant.
    setTimeout(() => { runningMacro.value = null }, Math.max(0, 180 - (Date.now() - started)))
  }
}
const mediaKeyIcons: Record<string, unknown> = {
  PrevTrack: SkipBack, PlayPause: Play, NextTrack: SkipForward, Stop: Square,
  Mute: VolumeX, VolumeDown: Volume1, VolumeUp: Volume2,
}
const mediaKeyLabelIds: Record<string, string> = {
  PrevTrack: 'prevTrack', PlayPause: 'playPause', NextTrack: 'nextTrack', Stop: 'stop',
  Mute: 'mute', VolumeDown: 'volumeDown', VolumeUp: 'volumeUp',
}

async function onMediaKeyPress(key: string) {
  if (key in consumerKeys) {
    const usage = consumerKeys[key as ConsumerKeyName]
    try {
      await hidApi.consumer(usage)
    } catch (err) {
      console.error('[VirtualKeyboard] Media key send failed:', err)
    }
  }
}

function switchOsLayout(os: KeyboardOsType) {
  selectedOs.value = os
  localStorage.setItem('vkb-os-layout', os)
  updateKeyboardLayout()
}

function updateKeyboardLayout() {
  const bottomRow = getBottomRow()
  const baseLayout = isCompactLayout.value ? compactMainLayout : keyboardLayout.main
  const newLayout = {
    ...baseLayout,
    default: [
      ...baseLayout.default.slice(0, -1),
      bottomRow,
    ],
    shift: [
      ...baseLayout.shift.slice(0, -1),
      bottomRow,
    ],
  }
  mainKeyboard.value?.setOptions({ layout: newLayout, display: keyDisplayMap.value })
  updateKeyboardButtonTheme()
}

function flashKey(key: string) {
  flashedKeys.value = [...flashedKeys.value, key]
  updateKeyboardButtonTheme()
  setTimeout(() => {
    flashedKeys.value = flashedKeys.value.filter(k => k !== key)
    updateKeyboardButtonTheme()
  }, FLASH_MS)
}

function setLatches(next: Map<CanonicalKey, ModifierLatch>) {
  modifierLatches.value = next
  pressedModifiers.value = [...next.keys()].reduce((mask, key) => mask | (modifiers[key] ?? 0), 0)
  updateKeyboardButtonTheme()
}

async function releaseModifiers(keysToRelease: CanonicalKey[], remaining: Map<CanonicalKey, ModifierLatch>) {
  setLatches(remaining)
  for (const keyCode of keysToRelease) {
    await sendKeyPress(keyCode, false, pressedModifiers.value)
    emit('keyUp', keyCode)
  }
}

/** Releases one-shot modifiers after the key they were armed for. */
async function consumeArmedModifiers() {
  const { kept, released } = consumeArmed(modifierLatches.value)
  if (released.length) await releaseModifiers(released, kept)
}

async function releaseAllModifiers() {
  const held = [...modifierLatches.value.keys()]
  if (held.length) await releaseModifiers(held, new Map())
}

async function tapModifierKey(keyCode: CanonicalKey) {
  const now = Date.now()
  const current = modifierLatches.value.get(keyCode)
  const next = tapModifier(current, now - (lastModifierTap.get(keyCode) ?? 0))
  lastModifierTap.set(keyCode, now)
  const latches = new Map(modifierLatches.value)
  if (next) latches.set(keyCode, next)
  else latches.delete(keyCode)
  if (!current) {
    setLatches(latches)
    await sendKeyPress(keyCode, true, pressedModifiers.value)
    emit('keyDown', keyCode)
  } else if (!next) {
    await releaseModifiers([keyCode], latches)
  } else {
    setLatches(latches)
  }
}

async function onKeyDown(key: string) {
  const cleanKey = key.replace(/[()]/g, '')

  if (capture.value) {
    flashKey(key)
    captureInto(cleanKey)
    return
  }

  if (!(cleanKey in keys)) {
    console.warn(`[VirtualKeyboard] Unknown key: ${cleanKey}`)
    return
  }

  const keyCode = keys[cleanKey as KeyName]

  if ((modifiers[keyCode] ?? 0) !== 0) {
    await tapModifierKey(keyCode)
    return
  }

  flashKey(key)

  if (latchingKeys.some(latchingKey => latchingKey === keyCode)) {
    emit('keyDown', keyCode)
    const currentMask = pressedModifiers.value & 0xff
    await sendKeyPress(keyCode, true, currentMask)
    setTimeout(() => {
      sendKeyPress(keyCode, false, currentMask)
      emit('keyUp', keyCode)
    }, 100)
    return
  }

  keysDown.value.push(keyCode)
  emit('keyDown', keyCode)
  const currentMask = pressedModifiers.value & 0xff
  await sendKeyPress(keyCode, true, currentMask)
  updateKeyboardButtonTheme()
  setTimeout(async () => {
    keysDown.value = keysDown.value.filter(k => k !== keyCode)
    await sendKeyPress(keyCode, false, currentMask)
    emit('keyUp', keyCode)
    updateKeyboardButtonTheme()
    await consumeArmedModifiers()
  }, 50)
}

async function onKeyUp() {
}

async function sendKeyPress(keyCode: CanonicalKey, press: boolean, modifierMask: number) {
  try {
    await hidApi.keyboard(press ? 'down' : 'up', keyCode, modifierMask & 0xff)
  } catch (err) {
    console.error('[VirtualKeyboard] Key send failed:', err)
  }
}

interface MacroStep {
  keys: string[]
  modifiers: string[]
}

async function executeMacro(steps: MacroStep[]) {
  let macroModifierMask = pressedModifiers.value & 0xff

  for (const step of steps) {
    // Modifiers the user already holds stay down; the macro only adds the missing ones.
    const added = step.modifiers.filter(mod => mod in keys && (pressedModifiers.value & (modifiers[keys[mod as KeyName]] ?? 0)) === 0)
    for (const mod of added) {
      if (mod in keys) {
        const modHid = keys[mod as KeyName]
        macroModifierMask = updateModifierMaskForKey(macroModifierMask, modHid, true)
        await sendKeyPress(modHid, true, macroModifierMask)
      }
    }

    for (const key of step.keys) {
      if (key in keys) {
        await sendKeyPress(keys[key as KeyName], true, macroModifierMask)
      }
    }

    await new Promise(resolve => setTimeout(resolve, 50))

    for (const key of step.keys) {
      if (key in keys) {
        await sendKeyPress(keys[key as KeyName], false, macroModifierMask)
      }
    }

    for (const mod of added) {
      if (mod in keys) {
        const modHid = keys[mod as KeyName]
        macroModifierMask = updateModifierMaskForKey(macroModifierMask, modHid, false)
        await sendKeyPress(modHid, false, macroModifierMask)
      }
    }
  }
}

const functionKeys = [
  'Escape', 'Tab', 'CapsLock', 'Enter', 'Backspace', 'ShiftLeft', 'ShiftRight',
  'ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight', 'ContextMenu',
  'F1', 'F2', 'F3', 'F4', 'F5', 'F6', 'F7', 'F8', 'F9', 'F10', 'F11', 'F12',
  'PrintScreen', 'ScrollLock', 'Pause', 'Insert', 'Delete', 'Home', 'End', 'PageUp', 'PageDown',
  'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
].join(' ')

function updateKeyboardButtonTheme() {
  const downKeys = keyNamesForDownKeys.value.join(' ')
  const buttonTheme = [
    {
      class: 'vkb-fn',
      buttons: functionKeys,
    },
    {
      class: 'down-key',
      buttons: downKeys,
    },
    { class: 'vkb-armed', buttons: latchedKeyNames('armed') },
    { class: 'vkb-locked', buttons: latchedKeyNames('locked') },
  ]

  mainKeyboard.value?.setOptions({ buttonTheme: buttonTheme.filter(theme => theme.buttons) })
}

watch([() => props.pressedKeys, capture], updateKeyboardButtonTheme)

watch([layoutName, () => props.capsLock], ([name]) => {
  mainKeyboard.value?.setOptions({
    layoutName: name,
    display: keyDisplayMap.value,
  })
  updateKeyboardButtonTheme()
})

function initKeyboards() {
  const id = keyboardId.value

  const mainEl = document.querySelector(`#${id}-main`)

  if (!mainEl) {
    console.warn('[VirtualKeyboard] DOM elements not ready, retrying...', id)
    setTimeout(initKeyboards, 50)
    return
  }

  mainKeyboard.value = new Keyboard(mainEl, {
    layout: isCompactLayout.value ? compactMainLayout : keyboardLayout.main,
    layoutName: layoutName.value,
    display: keyDisplayMap.value,
    theme: 'hg-theme-default hg-layout-default vkb-keyboard',
    onKeyPress: onKeyDown,
    onKeyReleased: onKeyUp,
    buttonTheme: [{ class: 'vkb-fn', buttons: functionKeys }],
    disableButtonHold: true,
    preventMouseDownDefault: true,
    preventMouseUpDefault: true,
    stopMouseDownPropagation: true,
    stopMouseUpPropagation: true,
  })

  updateKeyboardLayout()
  console.log('[VirtualKeyboard] Keyboards initialized:', id)
}

function destroyKeyboards() {
  mainKeyboard.value?.destroy()
  mainKeyboard.value = null
}

function clampPosition(x: number, y: number) {
  const rect = keyboardRef.value?.getBoundingClientRect()
  const width = rect?.width ?? 0
  const height = rect?.height ?? 0
  return {
    x: Math.min(Math.max(0, window.innerWidth - width), Math.max(0, x)),
    y: Math.min(Math.max(0, window.innerHeight - height), Math.max(0, y)),
  }
}

function placeFloating() {
  const rect = keyboardRef.value?.getBoundingClientRect()
  if (!rect) return
  const saved = JSON.parse(localStorage.getItem('vkbFloatingPosition') || 'null') as { x: number; y: number } | null
  position.value = clampPosition(
    saved?.x ?? (window.innerWidth - rect.width) / 2,
    saved?.y ?? window.innerHeight - rect.height - 24,
  )
}

function startDrag(e: PointerEvent) {
  if (isAttached.value || !keyboardRef.value || e.button !== 0) return
  if ((e.target as HTMLElement).closest('button')) return
  const handle = e.currentTarget as HTMLElement
  handle.setPointerCapture(e.pointerId)
  isDragging.value = true
  dragOffset.value = { x: e.clientX - position.value.x, y: e.clientY - position.value.y }
  const move = (event: PointerEvent) => {
    position.value = clampPosition(event.clientX - dragOffset.value.x, event.clientY - dragOffset.value.y)
  }
  const end = () => {
    isDragging.value = false
    localStorage.setItem('vkbFloatingPosition', JSON.stringify(position.value))
    handle.removeEventListener('pointermove', move)
    handle.removeEventListener('pointerup', end)
    handle.removeEventListener('pointercancel', end)
  }
  handle.addEventListener('pointermove', move)
  handle.addEventListener('pointerup', end)
  handle.addEventListener('pointercancel', end)
}

function onWindowResize() {
  if (!isAttached.value) position.value = clampPosition(position.value.x, position.value.y)
}

async function toggleAttached() {
  destroyKeyboards()
  isAttached.value = !isAttached.value
  emit('update:attached', isAttached.value)

  await nextTick()
  await nextTick() // Extra tick for Teleport

  setTimeout(() => {
    initKeyboards()
    if (!isAttached.value) nextTick(placeFloating)
  }, 100)
}

function close() {
  stopCapture()
  releaseAllModifiers()
  emit('update:visible', false)
}

function onPageHidden() {
  if (document.visibilityState === 'hidden') releaseAllModifiers()
}

watch(() => props.visible, async (visible) => {
  console.log('[VirtualKeyboard] Visibility changed:', visible, 'attached:', isAttached.value, 'id:', keyboardId.value)
  if (visible) {
    await nextTick()
    initKeyboards()
    if (!isAttached.value) nextTick(placeFloating)
  } else {
    destroyKeyboards()
  }
}, { immediate: true })

watch(() => props.attached, (value) => {
  if (value !== undefined) {
    isAttached.value = value
  }
})

onMounted(() => {
  const savedOs = localStorage.getItem('vkb-os-layout') as KeyboardOsType | null
  if (savedOs && ['windows', 'mac', 'android'].includes(savedOs)) {
    selectedOs.value = savedOs
  }

  if (window.matchMedia) {
    compactLayoutMedia = window.matchMedia('(max-width: 640px)')
    setCompactLayout(compactLayoutMedia.matches)
    compactLayoutListener = (event: MediaQueryListEvent) => {
      setCompactLayout(event.matches)
    }
    compactLayoutMedia.addEventListener('change', compactLayoutListener)
  }

  window.addEventListener('resize', onWindowResize)
  document.addEventListener('visibilitychange', onPageHidden)
})

onUnmounted(() => {
  if (compactLayoutMedia && compactLayoutListener) {
    compactLayoutMedia.removeEventListener('change', compactLayoutListener)
  }
  window.removeEventListener('resize', onWindowResize)
  document.removeEventListener('visibilitychange', onPageHidden)
  stopCapture()
  releaseAllModifiers()
  destroyKeyboards()
})
</script>

<template>
  <Transition name="keyboard-fade">
    <div
      v-if="visible"
      :id="keyboardId"
      ref="keyboardRef"
      :data-console-layout="consoleAppearance"
      data-virtual-keyboard
      class="vkb"
      :class="{
        'vkb--attached': isAttached,
        'vkb--floating': !isAttached,
        'vkb--dragging': isDragging,
        'vkb--compact': isCompactLayout,
      }"
      :style="!isAttached ? { transform: `translate(${position.x}px, ${position.y}px)` } : undefined"
      :aria-label="t('virtualKeyboard.title')"
      role="region"
    >
      <div class="vkb-bar" @pointerdown="startDrag">
        <span class="vkb-title" :class="{ 'vkb-title--combo': comboText }"><KeyboardIcon class="size-4 shrink-0" /><span class="vkb-title__text">{{ t('virtualKeyboard.title') }}</span></span>
        <div class="vkb-segmented" role="radiogroup" :aria-label="t('virtualKeyboard.layout')">
          <button
            v-for="os in (['windows', 'mac', 'android'] as KeyboardOsType[])"
            :key="os"
            type="button"
            role="radio"
            :aria-checked="selectedOs === os"
            @click="switchOsLayout(os)"
          >
            {{ os === 'windows' ? 'Win' : os === 'mac' ? 'Mac' : 'Android' }}
          </button>
        </div>
        <div v-if="comboText" class="vkb-combo" role="status" :title="t('virtualKeyboard.stickyHint')">
          <span class="vkb-combo__keys">{{ comboText }} + …</span>
          <button type="button" @click="releaseAllModifiers">{{ t('virtualKeyboard.releaseAll') }}</button>
        </div>
        <div class="vkb-tools">
          <div v-if="capture" class="vkb-capture" role="group" :aria-label="t('virtualKeyboard.recordShortcut')">
            <span class="vkb-capture__dot" aria-hidden="true" />
            <span class="vkb-capture__chord" :class="{ 'vkb-capture__chord--empty': !captureLabel }" aria-live="polite">{{ captureLabel || t('virtualKeyboard.pressChord') }}</span>
            <input v-model="capture.name" data-shortcut-name class="vkb-capture__name" maxlength="24" :placeholder="t('virtualKeyboard.shortcutName')" :aria-label="t('virtualKeyboard.shortcutName')">
            <span v-if="captureDuplicate" class="vkb-capture__error">{{ t('virtualKeyboard.duplicate') }}</span>
            <button type="button" class="vkb-chip vkb-chip--primary" :disabled="!captureLabel || captureDuplicate" @click="commitCapture">{{ t('virtualKeyboard.save') }}</button>
            <button type="button" class="vkb-chip" @click="stopCapture">{{ t('virtualKeyboard.cancel') }}</button>
          </div>
          <div v-else class="vkb-group" :aria-label="t('virtualKeyboard.macros')" role="group">
            <button
              v-for="shortcut in shortcuts"
              :key="shortcut.id"
              type="button"
              class="vkb-chip"
              :class="{ 'vkb-chip--running': runningMacro === shortcut.id }"
              :aria-disabled="runningMacro !== null"
              :title="shortcut.name ? chordLabel(shortcut.keys, selectedOs) : undefined"
              @click="runShortcut(shortcut)"
            >
              {{ shortcutText(shortcut) }}
            </button>
            <button
              type="button"
              class="vkb-icon"
              :disabled="customShortcuts.length >= MAX_CUSTOM_SHORTCUTS"
              :aria-label="t('virtualKeyboard.addShortcut')"
              :title="customShortcuts.length >= MAX_CUSTOM_SHORTCUTS ? t('virtualKeyboard.shortcutLimit', { n: MAX_CUSTOM_SHORTCUTS }) : t('virtualKeyboard.addShortcut')"
              @click="startCapture"
            >
              <Plus class="size-3.5" />
            </button>
            <DropdownMenu>
              <DropdownMenuTrigger as-child>
                <button type="button" class="vkb-icon" :aria-label="t('virtualKeyboard.allShortcuts')" :title="t('virtualKeyboard.allShortcuts')">
                  <Ellipsis class="size-3.5" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" class="vkb-menu w-64">
                <DropdownMenuLabel class="text-xs text-muted-foreground">{{ t('virtualKeyboard.presetShortcuts') }}</DropdownMenuLabel>
                <DropdownMenuItem v-for="shortcut in PRESET_SHORTCUTS[selectedOs]" :key="shortcut.id" @select="runShortcut(shortcut)">
                  <span class="font-mono text-xs">{{ chordLabel(shortcut.keys, selectedOs) }}</span>
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuLabel class="text-xs text-muted-foreground">{{ t('virtualKeyboard.customShortcuts') }}</DropdownMenuLabel>
                <p v-if="!customShortcuts.length" class="px-2 pb-1.5 text-xs text-muted-foreground">{{ t('virtualKeyboard.noCustomShortcuts') }}</p>
                <DropdownMenuItem v-for="shortcut in customShortcuts" :key="shortcut.id" class="group" @select="runShortcut(shortcut)">
                  <span class="min-w-0 flex-1 truncate" :class="{ 'font-mono text-xs': !shortcut.name }">{{ shortcutText(shortcut) }}</span>
                  <span v-if="shortcut.name" class="shrink-0 font-mono text-xs text-muted-foreground">{{ chordLabel(shortcut.keys, selectedOs) }}</span>
                  <button
                    type="button"
                    class="vkb-menu-delete"
                    :aria-label="t('virtualKeyboard.deleteShortcut', { name: shortcutText(shortcut) })"
                    :title="t('virtualKeyboard.deleteShortcut', { name: shortcutText(shortcut) })"
                    @pointerdown.stop
                    @pointerup.stop
                    @click.stop.prevent="deleteShortcut(shortcut.id)"
                  >
                    <Trash2 class="size-3.5" />
                  </button>
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem :disabled="customShortcuts.length >= MAX_CUSTOM_SHORTCUTS" @select="startCapture">
                  <Plus class="size-3.5" />{{ t('virtualKeyboard.addShortcut') }}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
          <template v-if="props.consumerEnabled !== false && !capture">
            <span class="vkb-divider" aria-hidden="true" />
            <div class="vkb-group" :aria-label="t('virtualKeyboard.media')" role="group">
              <button
                v-for="key in mediaKeys"
                :key="key"
                type="button"
                class="vkb-icon"
                :aria-label="t(`virtualKeyboard.${mediaKeyLabelIds[key]}`)"
                :title="t(`virtualKeyboard.${mediaKeyLabelIds[key]}`)"
                @click="onMediaKeyPress(key)"
              >
                <component :is="mediaKeyIcons[key]" class="size-3.5" />
              </button>
            </div>
          </template>
        </div>
        <div class="vkb-actions">
          <button
            type="button"
            class="vkb-icon vkb-dock-toggle"
            :aria-label="t(isAttached ? 'virtualKeyboard.detach' : 'virtualKeyboard.attach')"
            :title="t(isAttached ? 'virtualKeyboard.detach' : 'virtualKeyboard.attach')"
            @click="toggleAttached"
          >
            <component :is="isAttached ? PictureInPicture2 : PanelBottom" class="size-3.5" />
          </button>
          <Button variant="ghost" size="icon-sm" :aria-label="t('virtualKeyboard.hide')" :title="t('virtualKeyboard.hide')" @click="close">
            <X class="size-3.5" />
          </Button>
        </div>
      </div>
      <div class="vkb-body">
        <div :id="`${keyboardId}-main`" class="kb-main-container"></div>
      </div>
    </div>
  </Transition>
</template>

<style>
/* simple-keyboard renders into .kb-main-container without a .simple-keyboard class. */
.vkb .hg-theme-default {
  --vkb-gap: 4px;
  width: 100%;
  padding: 0;
  border-radius: 0;
  background: transparent;
  font-family: inherit;
  overflow: visible;
}
.vkb .hg-theme-default .hg-row:not(:last-child) { margin-bottom: var(--vkb-gap); }
.vkb .hg-theme-default .hg-row .hg-button:not(:last-child) { margin-right: var(--vkb-gap); }
.vkb .hg-theme-default .hg-row:first-child { margin-bottom: calc(var(--vkb-gap) * 2); }
.vkb--compact .hg-theme-default .hg-row:first-child { margin-bottom: var(--vkb-gap); }

.vkb .hg-theme-default .hg-button {
  height: var(--vkb-key-h, 32px);
  min-width: 0;
  flex: 1 1 0;
  padding: 0 4px;
  border: 1px solid var(--border);
  border-bottom-color: color-mix(in oklch, var(--border), var(--foreground) 18%);
  border-radius: 6px;
  background: var(--background);
  color: var(--foreground);
  box-shadow: none;
  font-size: 12px;
  font-weight: 500;
  line-height: 1;
  transition: background-color 80ms, color 80ms;
}
.vkb .hg-theme-default .hg-button span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.vkb .hg-theme-default .hg-button.vkb-fn {
  background: var(--muted);
  color: var(--muted-foreground);
  font-size: 11px;
}
.vkb .hg-theme-default .hg-button:hover { background: var(--accent); color: var(--accent-foreground); }
/* One-shot modifier: tinted until the next key consumes it. */
.vkb .hg-theme-default .hg-button.vkb-armed {
  background: color-mix(in oklch, var(--primary) 14%, var(--background));
  border-color: var(--primary);
  color: var(--primary);
  font-weight: 600;
}
/* Locked modifier: solid, with a lock bar like a Caps Lock light. */
.vkb .hg-theme-default .hg-button.vkb-locked {
  position: relative;
  background: var(--primary);
  border-color: var(--primary);
  color: var(--primary-foreground);
  font-weight: 600;
}
.vkb .hg-theme-default .hg-button.vkb-locked::after {
  content: '';
  position: absolute;
  top: 4px;
  left: 50%;
  width: 12px;
  height: 2px;
  border-radius: 1px;
  background: currentColor;
  transform: translateX(-50%);
  opacity: .8;
}
.vkb .hg-theme-default .hg-button:active,
.vkb .hg-theme-default .hg-button.hg-activeButton,
.vkb .hg-theme-default .hg-button.down-key {
  background: var(--primary);
  border-color: var(--primary);
  color: var(--primary-foreground);
}

/* Proportions follow a physical keyboard, in key units. */
.vkb .hg-theme-default .hg-button[data-skbtn="Backspace"] { flex-grow: 2; }
.vkb .hg-theme-default .hg-button[data-skbtn="Tab"],
.vkb .hg-theme-default .hg-button[data-skbtn="Backslash"],
.vkb .hg-theme-default .hg-button[data-skbtn="(Backslash)"] { flex-grow: 1.5; }
.vkb .hg-theme-default .hg-button[data-skbtn="CapsLock"] { flex-grow: 1.75; }
.vkb .hg-theme-default .hg-button[data-skbtn="Enter"],
.vkb .hg-theme-default .hg-button[data-skbtn="ShiftLeft"] { flex-grow: 2.25; }
.vkb .hg-theme-default .hg-button[data-skbtn="ShiftRight"] { flex-grow: 1.75; }
.vkb .hg-theme-default .hg-button[data-skbtn^="Control"],
.vkb .hg-theme-default .hg-button[data-skbtn^="Meta"],
.vkb .hg-theme-default .hg-button[data-skbtn^="Alt"],
.vkb .hg-theme-default .hg-button[data-skbtn="ContextMenu"] { flex-grow: 1.25; }
.vkb .hg-theme-default .hg-button[data-skbtn="Space"] { flex-grow: 6; }
.vkb .hg-theme-default .hg-button[data-skbtn^="Arrow"] { font-size: 13px; }
/* Gap before the navigation cluster and the arrow keys. */
.vkb:not(.vkb--compact) .hg-theme-default .hg-button[data-skbtn="PrintScreen"],
.vkb:not(.vkb--compact) .hg-theme-default .hg-button[data-skbtn="Insert"],
.vkb:not(.vkb--compact) .hg-theme-default .hg-button[data-skbtn="Delete"],
.vkb .hg-theme-default .hg-button[data-skbtn="ArrowUp"],
.vkb .hg-theme-default .hg-button[data-skbtn="ArrowLeft"] { margin-left: calc(var(--vkb-gap) * 2); }
.vkb--compact .hg-theme-default .hg-button[data-skbtn="ArrowUp"],
.vkb--compact .hg-theme-default .hg-button[data-skbtn="ArrowLeft"] { margin-left: 0; }
.vkb--compact .hg-theme-default .hg-button { padding: 0 1px; font-size: 13px; }
.vkb--compact .hg-theme-default .hg-button.vkb-fn { font-size: 11px; }
.vkb--compact .hg-theme-default .hg-button[data-skbtn="Space"] { flex-grow: 3; }
</style>

<style scoped>
.vkb {
  z-index: 100;
  user-select: none;
  color: var(--foreground);
  background: var(--background);
  border-top: 1px solid var(--border);
}
.vkb--attached { position: relative; width: 100%; }
.vkb--floating {
  position: fixed;
  top: 0;
  left: 0;
  width: min(860px, calc(100vw - 16px));
  border: 1px solid var(--border);
  border-radius: 12px;
  background: color-mix(in srgb, var(--background) 96%, transparent);
  backdrop-filter: blur(12px);
  box-shadow: 0 12px 32px rgb(0 0 0 / 18%);
  --vkb-key-h: 30px;
}
.vkb--dragging { opacity: .9; }

.vkb-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 36px;
  padding: 0 4px 0 12px;
  font-size: 12px;
}
.vkb--floating .vkb-bar { cursor: grab; touch-action: none; }
.vkb--dragging .vkb-bar { cursor: grabbing; }
.vkb-title { display: inline-flex; align-items: center; gap: 6px; flex-shrink: 0; font-weight: 600; }

.vkb-segmented {
  display: inline-flex;
  flex-shrink: 0;
  padding: 2px;
  border-radius: 6px;
  background: var(--muted);
}
.vkb-segmented button {
  height: 22px;
  padding: 0 8px;
  border-radius: 4px;
  color: var(--muted-foreground);
  font-size: 11px;
  font-weight: 500;
}
.vkb-segmented button:hover { color: var(--foreground); }
.vkb-segmented button[aria-checked='true'] {
  background: var(--background);
  color: var(--foreground);
  box-shadow: 0 1px 2px rgb(0 0 0 / 10%);
}

.vkb-tools {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  margin-left: auto;
  overflow-x: auto;
  scrollbar-width: none;
}
.vkb-tools::-webkit-scrollbar { display: none; }
.vkb-group { display: flex; align-items: center; gap: 2px; flex-shrink: 0; }
.vkb-divider { width: 1px; height: 16px; flex-shrink: 0; background: var(--border); }
.vkb-chip {
  height: 24px;
  padding: 0 8px;
  border: 1px solid var(--border);
  border-radius: 6px;
  font-size: 11px;
  font-weight: 500;
  white-space: nowrap;
}
.vkb-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  flex-shrink: 0;
  border-radius: 6px;
  color: var(--muted-foreground);
}
.vkb-chip:hover, .vkb-icon:hover { background: var(--accent); color: var(--accent-foreground); }
.vkb-chip:active, .vkb-icon:active, .vkb-chip--running, .vkb-chip--running:hover {
  background: var(--primary);
  border-color: var(--primary);
  color: var(--primary-foreground);
}
.vkb-chip[aria-disabled='true']:not(.vkb-chip--running) { opacity: .5; pointer-events: none; }
.vkb-chip--primary { background: var(--primary); border-color: var(--primary); color: var(--primary-foreground); }
.vkb-chip--primary:hover { background: color-mix(in oklch, var(--primary) 88%, var(--foreground)); color: var(--primary-foreground); }
.vkb-chip:disabled, .vkb-icon:disabled { opacity: .4; pointer-events: none; }
.vkb-capture .vkb-chip { flex-shrink: 0; }
.vkb-capture { display: flex; flex: 1; align-items: center; justify-content: flex-end; gap: 6px; min-width: 0; }
.vkb-capture__dot { width: 8px; height: 8px; border-radius: 9999px; background: var(--destructive); animation: vkb-pulse 1.2s ease-in-out infinite; }
.vkb-capture__chord { flex-shrink: 0; min-width: 96px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; font-weight: 600; }
.vkb-capture__chord--empty { color: var(--muted-foreground); font-family: inherit; font-weight: 400; }
.vkb-capture__name {
  width: 120px; min-width: 64px; flex-shrink: 1; height: 24px; padding: 0 8px;
  border: 1px solid var(--border); border-radius: 6px; background: var(--background);
  font-size: 12px; user-select: text;
}
.vkb-capture__name:focus-visible { outline: 2px solid var(--ring); outline-offset: -1px; }
.vkb-capture__error { color: var(--destructive); font-size: 11px; white-space: nowrap; }
@keyframes vkb-pulse { 50% { opacity: .3; } }
@media (prefers-reduced-motion: reduce) { .vkb-capture__dot { animation: none; } }
.vkb-menu-delete {
  display: inline-flex; align-items: center; justify-content: center;
  width: 24px; height: 24px; margin: -4px -4px -4px 0; flex-shrink: 0;
  border-radius: 4px; color: var(--muted-foreground);
}
.vkb-menu-delete:hover { background: color-mix(in oklch, var(--destructive) 12%, transparent); color: var(--destructive); }
.vkb-combo {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
  height: 24px;
  padding: 0 2px 0 8px;
  border-radius: 6px;
  background: color-mix(in oklch, var(--primary) 12%, var(--background));
  color: var(--primary);
  font-size: 11px;
  font-weight: 600;
  white-space: nowrap;
}
.vkb-combo button {
  height: 20px;
  padding: 0 6px;
  border-radius: 4px;
  color: var(--foreground);
  font-weight: 500;
}
.vkb-combo button:hover { background: var(--background); }
.vkb-chip:focus-visible, .vkb-icon:focus-visible, .vkb-segmented button:focus-visible {
  outline: 2px solid var(--ring);
  outline-offset: -2px;
}
.vkb-actions { display: flex; align-items: center; flex-shrink: 0; }
.vkb-actions::before { content: ''; width: 1px; height: 16px; margin-right: 4px; background: var(--border); }

.vkb-body {
  max-width: 1080px;
  margin: 0 auto;
  padding: 2px 8px 8px;
}

/* Phones: tools get their own scrollable row, touch-sized keys, no floating mode. */
@media (max-width: 767px) {
  .vkb-bar { flex-wrap: wrap; height: auto; row-gap: 0; padding: 0 4px 0 12px; }
  .vkb-title { height: 40px; }
  .vkb-actions { margin-left: auto; }
  .vkb-tools {
    order: 1; flex-basis: calc(100% + 16px); margin: 0 -4px 0 -12px; padding: 4px 32px 6px 12px;
    mask-image: linear-gradient(to right, #000 calc(100% - 32px), transparent);
  }
  .vkb-dock-toggle { display: none; }
  .vkb-title--combo .vkb-title__text { display: none; }
  .vkb-tools:has(.vkb-capture) { mask-image: none; padding-right: 12px; }
  .vkb-capture__chord { min-width: 0; }
  .vkb-capture__name { flex: 1; width: auto; }
  .vkb-capture__error { display: none; }
  .vkb-body { padding: 2px 4px calc(6px + env(safe-area-inset-bottom)); }
}
@media (pointer: coarse) {
  .vkb { --vkb-key-h: 40px; }
  .vkb-chip { height: 32px; padding: 0 12px; }
  .vkb-icon { width: 36px; height: 36px; }
}
@media (pointer: coarse) and (max-height: 500px) {
  .vkb { --vkb-key-h: 28px; }
  .vkb-bar { height: 36px; flex-wrap: nowrap; }
  .vkb-tools { order: 0; flex-basis: auto; margin: 0 0 0 auto; padding: 0; mask-image: none; }
  .vkb-title { height: auto; }
  .vkb-actions { margin-left: 0; }
  .vkb-chip { height: 28px; }
  .vkb-icon { width: 32px; height: 32px; }
}

.keyboard-fade-enter-active,
.keyboard-fade-leave-active { transition: opacity 150ms ease, transform 150ms ease; }
.keyboard-fade-enter-from,
.keyboard-fade-leave-to { opacity: 0; }
.vkb--attached.keyboard-fade-enter-from,
.vkb--attached.keyboard-fade-leave-to { transform: translateY(12px); }
</style>
