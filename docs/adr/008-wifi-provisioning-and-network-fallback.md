# ADR-008: Wifi provisioning and network fallback

## Status
Accepted — 2026-10-26

## Context

- The first external user needs wifi (#41).
- mdma-stage should be able to run on wifi while prod keeps using the
  single-port switch.

## Decision

1. **Provisioning flow.** Provisioning is always done over ethernet, with the
   beacon booted from the SD card at `welcome-to-mdma.local`. The form has an
   optional wifi section: SSID, passphrase and a required ISO country code.

2. **No custom fallback logic.** dhcpcd runs on all interfaces and gives wired
   interfaces a lower route metric than wireless. On prod, eth0's metric is
   1004; wireless gets a much higher one, and the lower metric wins. Ethernet
   is preferred while plugged in, and wifi takes over when it is not.

3. **wpa_supplicant setup.**
   - It runs under the stock Void runit service.
   - Provisioning writes `/etc/sv/wpa_supplicant/conf` containing
     `WPA_INTERFACE=wlan0`. The stock run script then executes
     `wpa_supplicant -M -c /etc/wpa_supplicant/wpa_supplicant.conf -i wlan0`.
   - `-M` makes it wait for wlan0 to appear.
   - The package does not own that `conf` file, so upgrades leave it alone.
     `wpa_supplicant.conf` is a conf_file and is preserved when modified.

4. **The plaintext passphrase never reaches the target.**
   - The beacon derives the WPA PSK with PBKDF2-HMAC-SHA1 (4096 iterations,
     salted with the SSID), as `wpa_passphrase` does.
   - Only the 64-hex PSK is written to `wpa_supplicant.conf`, at mode 0600,
     through an atomic temp file + rename (component `private_file`).
   - The SSID is written as hex, so no escaping is needed.
   - The passphrase and PSK are redacted from every Debug and log path,
     including the beacon `/logs` UI.
   - The wipe-override form never echoes the passphrase back; the user
     re-enters it.
   - Provisioning config types are intentionally not serde-serializable.

5. **Blank wifi on reprovision.**
   - Under Preserve with an intact root, existing wifi config is kept.
   - Under ReinstallOs or WipeAll, or when the root is reformatted, any
     previous wifi setup is removed, and the plan summary says so.

6. **Recovery.** If wifi fails (wrong passphrase, moved router, etc.), plug in
   ethernet and reprovision with the beacon.

## Consequences

### Negative
- WPA3-only (SAE) networks are not supported, because SAE needs the plaintext
  passphrase. WPA2 and WPA2/WPA3 mixed mode work.
- A single network only. Hidden networks, multiple networks, and changing wifi
  on an already-provisioned unit without reprovisioning are deferred.
- avahi publishes on both interfaces, so `<host>.local` may resolve to two
  addresses while both are up. There is a known risk of an avahi `-2` hostname
  rename, to be checked during acceptance.
- Wifi is hard-coded to interface `wlan0`.

## Related

- ADR-007 — provisioning data policy (`Preserve`, `ReinstallOs`, `WipeAll`)
