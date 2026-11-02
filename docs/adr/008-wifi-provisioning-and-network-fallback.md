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

## Amendment 2026-11-02: beacon reuses the installed system's wifi (#72)

The original text above is kept as written. Where it conflicts with this
amendment, the amendment wins.

### Why

- On wifi-only units (the first user), the beacon was unreachable. That broke
  service mode, recovery and reinstall, and made a wrong boot order
  unrecoverable.
- On a Pi 5 in an Argon NEO 5 M.2 case, the NVMe drops off PCIe while 2.4 GHz
  wifi is active (#69). Stage is stable with `freq_list` pinning wifi to 5 GHz
  plus `dtparam=pciex1_gen=1`.

### Decision

At startup, if eth0 has no carrier within 10 s (after `ip link set eth0 up`),
the beacon borrows the installed system's wifi config:

- It finds exactly one GPT partition named `root` (ext4, LABEL=root, not
  already mounted).
- It mounts that partition `ro,noload,nosuid,nodev,noexec`, refuses symlinks
  along the path to `etc/wpa_supplicant/wpa_supplicant.conf`, and caps the read
  at 64 KiB.
- It always unmounts, then copies the file VERBATIM to tmpfs at
  `/run/beacon/wifi/` (directory 0700, file 0600). Verbatim means hand-added
  lines such as `freq_list` survive.
- It starts the `beacon-wifi` runit service. The service ships with a `down`
  file and runs `wpa_supplicant -M -c /run/beacon/wifi/... -i wlan0`, without
  debug flags.
- Credentials never land on the SD card, the stock wpa_supplicant service stays
  disabled on the beacon, and /music is never mounted.
- Ethernet still wins. With a link, the NVMe is never touched, and dhcpcd route
  metrics prefer wired.
- Only psk and open networks are supported (no EAP or SAE), as before.

### Amended clauses

- **§1:** provisioning over wifi is possible but discouraged. The UI warns about
  the NVMe dropout.
- **§2:** "No custom fallback logic" no longer holds for the beacon. Route
  preference is still left to dhcpcd, but the beacon now has startup logic to
  bring wifi up when there is no wired link.
- **§5:**
  - When the wifi fields are blank and the beacon's network came from the
    installed system, ReinstallOs, WipeAll, and Preserve when stage 3
    reformats root all carry the installed conf over verbatim.
  - Preserve with root kept touches nothing wifi-related: no files are written
    and no service is enabled or disabled.
  - Half-filled wifi fields are rejected and never fall back to the
    carried-over conf. Explicit, complete form fields win.
  - The carried-over conf is the one parsed and validated at beacon startup. It
    is not re-read from disk.
  - **Escape route:** to provision WITHOUT wifi on a unit whose beacon came up
    on the installed system's wifi, boot the beacon with ethernet attached.
    Blank fields then mean no wifi.
  - Future option, not done: a three-way choice (Keep current / New network /
    No wifi) mapping onto the `WifiSetup` variants.
- **§6:** recovery on a wifi-only unit works without ethernet.

### Testing note

Side effects go through a crate-private `Host` trait in `network_fallback.rs`.
This is a test seam, not a component interface, so it is distinct from the
project's build-time polymorphism rule for components.

### Future options (not done)

- Write credentials to the beacon SD at provisioning time.
- Access-point fallback.

## Related

- ADR-007 — provisioning data policy (`Preserve`, `ReinstallOs`, `WipeAll`)
