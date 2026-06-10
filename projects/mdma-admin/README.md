# mdma-admin

System-level operations service for MDMA. Runs as `root` and owns operations that require elevated privileges: EEPROM writes and system reboot.

## What it does

- **Service mode** — writes `BOOT_ORDER=0xf461` (SD→USB→NVMe) to Pi 5 EEPROM so the next reboot lands on the beacon SD card. Used to trigger a reprovision without physical access or SSH. Refuses to enable service mode unless `PCIE_PROBE=1` is already set, preserving the return path to NVMe boot.
- **Reboot** — issues a graceful system reboot on request.

EEPROM helpers are provided by the shared `rpi_eeprom` component, also used by `mdma-beacon` stage5.

## IPC socket

```
/run/mdma/admin.sock   mode 0660, owner root:mdma
```

Only the gateway (`mdma-gateway`) communicates with this socket. Unprivileged processes cannot reach it directly.

## CLI

```bash
mdma admin service-mode status    # show current BOOT_ORDER and service-mode state
mdma admin service-mode enable    # flip BOOT_ORDER to SD-first; reboot to take effect
mdma admin service-mode disable   # restore BOOT_ORDER to 0xf164 (USB→NVMe→SD)
mdma admin reboot                 # graceful system reboot
```

## Web console

`/admin` page on the web console exposes status, enable, disable, and reboot buttons. Destructive actions require a confirmation step.

## Build

```bash
# From workspace root (devenv shell)
cargo polylith cargo --profile production build -p mdma-admin
```

Cross-compile for Pi 5:

```bash
just beacon-cross   # builds all Pi targets including mdma-admin
just deploy-dev     # deploy to welcome-to-mdma.local
```

---

[Back to root README](../../README.md)
