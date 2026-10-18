# UU Remote frontend verification

The terminal configuration and endpoints follow the shared UU terminal design.
Only the frontend is changed. No board connection, deployment, push, or Rust
changes are part of this work.

## Build

`pnpm install --frozen-lockfile` cannot run because this checkout contains
`package-lock.json`, not `pnpm-lock.yaml`. Use the commands documented in
`VIM4.md`, preserving the existing lockfile:

```sh
cd web
npm ci --ignore-scripts
npm exec -- vue-tsc -b
npm run build
```

There is no frontend unit test framework in this checkout. No framework or
runtime dependency was added. Production builds retain the existing warning
about chunks larger than 500 kB.

## Local browser checks

A temporary Vue fixture and Chrome DevTools Protocol driver exercised the real
components and request wrapper against simulated API responses. The fixture was
removed after verification; these checks do not establish backend behavior.

Verified:

- Desktop system changes wait for confirmation with a connected session;
  cancellation and failed PATCH requests restore the saved value.
- Polling does not overwrite an edited name; Escape restores the latest saved
  name. SSH validation rejects invalid hosts, and failed saves restore SSH text.
- Disabling a target with sessions requires confirmation; cancellation sends no
  PATCH, and failed saves leave the target enabled.
- All ten SSH result states render; successful trust triggers another test;
  HTTP 409 displays the fingerprint change message.
- Ending a session requires the second click; a 204 response succeeds and is
  followed by a fresh status request.
- Automatic QR login starts on entry, cancels when hidden, and restarts on
  return. A delayed login POST is followed by DELETE before a new POST when the
  card is unmounted and mounted again.
- Stale status is visible. Chinese and English layouts do not overflow at
  390 px. Chinese connection, desktop, terminal and login layouts were visually
  inspected, including light and dark themes.
- Both locales contain the same UU translation keys, and static translation
  references resolve.

## Board acceptance checks

1. Bind, rescan, cancel, expire, and unbind a real UU account. Switch Windows/Mac
   identities and verify their independent credentials and default scroll direction.
2. With a desktop client connected, confirm a codec/system/scroll change and
   verify disconnection and the behavior after reconnecting. Check that video
   limits match the Video section and that the navigation link selects it.
3. Enable each terminal target; create, detach, reconnect, rename, and end
   sessions in the official client. Disable a target with sessions and verify
   only those sessions end.
4. Exercise serial unplug/replug, the occupied-session label, baud changes,
   and USB gadget login. Verify both copy buttons over the board's HTTP/HTTPS
   access paths.
5. Test password-only SSH, new/changed host keys, fingerprint races, and an
   unreachable host against the backend. Confirm the full copied public key is
   accepted by the target.
6. In HID settings, verify USB Serial contributes two endpoints, over-budget
   profiles cannot be saved, failures restore the saved profile, and the target
   enumerates the serial device after a successful save.
