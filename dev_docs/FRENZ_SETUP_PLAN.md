# FRENZ setup repair

User request: Bluetooth scan and selectable bands instead of manual IDs; prompt
configuration saving; diagnose generic startup ValueError without exposing keys.

1. Add credential-free sidecar BLE scan, bounded native command, and band picker.
   Preserve advertised names; reject ambiguous duplicate names (SDK connects by
   name). No automatic retry or connection. Scan must not start the SDK.
2. Batch only changed vault fields. Preserve encryption/work factor; optimize
   scrypt/Salsa20 in debug builds. Show save errors and prevent edits during save.
3. Report safe startup phase and allowlisted error reason; distinguish SDK import
   from package installation. Test and rebuild an explicitly titled dev app.

Validation: scanner fixtures/error cases, real BLE scan, UI selection and saving
regressions, full existing suites, then native scan/select/save. Do not claim a
successful device recording until actual stored samples have been inspected.

## Evidence (2026-11-05)

- 7 Python tests, 212 frontend tests passed (40 skipped), 200 Rust library tests
  passed (2 ignored). Svelte check: 0 errors / 0 warnings. Native app built.
- Direct and packaged BLE scans found FRENZI85; advertised name and address
  returned, no key or SDK initialization needed.
- Native UI scan/select passed after launching the built executable with host
  hardware access. CUA-launched instance reported Bluetooth off despite radio
  on; launch environment is a test-harness caveat, not proof the radio was off.
- Existing-credential save reproduced taking over 25 seconds. A scoped process
  stack sample showed Stronghold snapshot encryption in unoptimized scrypt /
  Salsa20 at ~98% one core. The updated native form saved changed stream options
  with unchanged credentials and closed within 636 ms. First changed-secret
  snapshot latency after optimization still needs measurement.
- The SDK error from the saved Bridge credential now reports rejection explicitly.
  SDK constructor accepted the known-working project .env key in the exact
  packaged Python environment in 4.15 seconds; no BLE acquisition was started by
  that comparison. Values were not logged. User credential replacement remains
  needed before the live recording test; expiry has not been established.
- Cold packaging caught a conflicting Bleak constraint; fixed to 1.0.1, as
  required by frenztoolkit 0.3.2. The corrected packaged scan succeeded.
- SDK import is now labeled Loading rather than Installing. Generic failures
  include the safe internal phase; arbitrary vendor exception text stays hidden.

Remaining: user enters working key in the test app, then verify acquisition and
SQLite recording. Windows scan/packaging and long recordings are not yet tested.
The distinct test window title is passed as a local build override; installed
production app and production window title are unchanged.
