#!/bin/sh
# Stubbed test for migrate-bootfs (issue #65). Runs the real script against
# fake findmnt/blkid/xbps-*/mount/... on PATH and temp-dir stand-ins for /boot,
# /boot/firmware, /etc/fstab (via the script's MIGRATE_* overrides).
#
# Usage: sh scripts/package/test-migrate-bootfs.sh
# Needs only POSIX sh plus coreutils/tar/diff/grep/awk. Nothing on the host is
# touched.

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
SCRIPT="$SCRIPT_DIR/../../void-packages/srcpkgs/mdma-admin/files/mdma-admin/migrate-bootfs"

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

FAILS=0
PASSES=0

ok() {
    PASSES=$((PASSES + 1))
    echo "ok   - $1"
}
not_ok() {
    FAILS=$((FAILS + 1))
    echo "FAIL - $1"
}

# expect_status <name> <expected-nonzero|zero>
expect_status() {
    name=$1 want=$2
    if [ "$want" = zero ] && [ "$RC" -eq 0 ]; then
        ok "$name: exits 0"
    elif [ "$want" = nonzero ] && [ "$RC" -ne 0 ]; then
        ok "$name: exits non-zero"
    else
        not_ok "$name: exit status $RC, wanted $want"
        sed 's/^/     | /' "$OUT" "$ERR"
    fi
}

# expect_contains <name> <file> <text>
expect_contains() {
    if grep -q -F -e "$3" "$2"; then
        ok "$1: contains '$3'"
    else
        not_ok "$1: '$3' not found in $2"
        sed 's/^/     | /' "$2"
    fi
}

# expect_absent <name> <file> <text>
expect_absent() {
    if grep -q -F -e "$3" "$2"; then
        not_ok "$1: unexpected '$3' in $2"
        sed 's/^/     | /' "$2"
    else
        ok "$1: no '$3'"
    fi
}

write_stub() {
    # write_stub <name> <body...>: a stub that logs its invocation first.
    name=$1
    shift
    {
        echo '#!/bin/sh'
        echo 'echo "'"$name"' $*" >>"$STUB_LOG"'
        printf '%s\n' "$@"
    } >"$BIN/$name"
    chmod +x "$BIN/$name"
}

# new_case <name>: fresh fake system under $ROOT/<name>.
new_case() {
    CASE=$1
    T="$ROOT/$CASE"
    BIN="$T/bin"
    mkdir -p "$BIN" "$T/boot" "$T/firmware" "$T/modules/6.12.95_1" "$T/backups" "$T/state"

    printf '%s\n' \
        'PARTUUID=uuid-p2 / ext4 defaults 0 1' \
        'PARTUUID=uuid-p1 /boot/firmware vfat defaults 0 2' >"$T/fstab"
    for d in boot firmware; do
        echo "console=tty1 root=PARTUUID=uuid-p2 rootfstype=ext4" >"$T/$d/cmdline.txt"
        echo "dtparam=audio=on" >"$T/$d/config.txt"
    done

    STUB_LOG="$T/calls.log"
    : >"$STUB_LOG"
    OUT="$T/out"
    ERR="$T/err"

    write_stub id 'echo 0'
    write_stub sync ':'
    write_stub mdma-admin 'cat'
    write_stub df 'echo Filesystem; echo "x 1 1 500 1% /"'
    write_stub blkid 'for a; do d=$a; done; echo "uuid-${d##*/}"'
    write_stub findmnt \
        'what=$3; path=$5' \
        'case "$what:$path" in' \
        '  FSTYPE:"$MIGRATE_BOOT")' \
        '    if [ -e "$STUB_STATE/mounted" ] || [ "${STUB_BOOT_VFAT:-0}" = 1 ]; then echo vfat; else echo ext4; fi ;;' \
        '  FSTYPE:"$MIGRATE_FIRMWARE") echo vfat ;;' \
        '  SOURCE:"$MIGRATE_BOOT"|SOURCE:"$MIGRATE_FIRMWARE") echo /dev/p1 ;;' \
        '  SOURCE:/) echo /dev/p2 ;;' \
        '  *) exit 1 ;;' \
        'esac'
    write_stub xbps-query \
        'case "$*" in' \
        '  "-p conf_files rpi-firmware") echo /boot/cmdline.txt ;;' \
        '  "-p pkgver rpi-kernel") echo rpi-kernel-6.12.95_1 ;;' \
        '  "-p pkgver rpi-firmware") echo rpi-firmware-1_1 ;;' \
        '  *) exit 1 ;;' \
        'esac'
    # The dry run prints one "<pkgver> <action> ..." line per package, like
    # xbps. STUB_XBPS_UPGRADE=1 makes it want to upgrade the kernel instead.
    write_stub xbps-install \
        'case " $* " in' \
        '  *" -n "*)' \
        '    [ "${STUB_XBPS_DRYRUN_FAIL:-0}" = 1 ] && { echo "repo unreachable"; exit 1; }' \
        '    if [ "${STUB_XBPS_UPGRADE:-0}" = 1 ]; then' \
        '      echo "rpi-kernel-6.13.0_1 update aarch64 https://repo 1 2"' \
        '      echo "rpi-firmware-1_1 reinstall aarch64 https://repo 1 2"' \
        '    else' \
        '      for p; do case $p in -*) ;; *) echo "$p reinstall aarch64 https://repo 1 2" ;; esac; done' \
        '    fi ;;' \
        'esac' \
        'exit 0'
    write_stub xbps-pkgdb \
        'if [ "$1" = rpi-firmware ]; then' \
        '  echo "rpi-firmware-1_1: file /boot/config.txt: hash mismatch"' \
        '  echo "rpi-firmware-1_1: file /boot/cmdline.txt: hash mismatch"' \
        '  exit 1' \
        'fi' \
        'exit 0'
    write_stub umount 'exit 0'
    write_stub mount \
        '[ "${STUB_MOUNT_FAIL:-0}" = 1 ] && exit 1' \
        'touch "$STUB_STATE/mounted"'
}

# run_script <args...>: run migrate-bootfs in the current case.
run_script() {
    RC=0
    env PATH="$BIN:$PATH" \
        STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" \
        MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
        MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
        sh "$SCRIPT" "$@" >"$OUT" 2>"$ERR" || RC=$?
}

# --- 1. cmdline naming a device node stops everything before any change ------

new_case device-node-cmdline
echo "console=tty1 root=/dev/nvme0n1p2 rootfstype=ext4" >"$T/firmware/cmdline.txt"
cp "$T/fstab" "$T/fstab.orig"
run_script --apply
expect_status "$CASE" nonzero
expect_contains "$CASE" "$ERR" "root=PARTUUID=uuid-p2"
expect_contains "$CASE" "$ERR" "Nothing was changed"
expect_absent "$CASE" "$STUB_LOG" "umount"
expect_absent "$CASE" "$STUB_LOG" "mount "
expect_absent "$CASE" "$STUB_LOG" "xbps-install -Syf"
cmp -s "$T/fstab" "$T/fstab.orig" && ok "$CASE: fstab untouched" || not_ok "$CASE: fstab modified"
[ ! -e "$T/backups/bootfs-pre-65.tar" ] && ok "$CASE: no backup written" || not_ok "$CASE: backup written"

# --- 2. failing xbps dry run stops everything before any change --------------

new_case xbps-dry-run-fails
RC=0
env PATH="$BIN:$PATH" STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" STUB_XBPS_DRYRUN_FAIL=1 \
    MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
    MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
    sh "$SCRIPT" --apply >"$OUT" 2>"$ERR" || RC=$?
expect_status "$CASE" nonzero
expect_contains "$CASE" "$ERR" "dry run failed"
expect_absent "$CASE" "$STUB_LOG" "umount"
[ ! -e "$T/backups/bootfs-pre-65.tar" ] && ok "$CASE: no backup written" || not_ok "$CASE: backup written"

# --- 3. dry run changes nothing ----------------------------------------------

new_case dry-run
run_script
expect_status "$CASE" zero
expect_contains "$CASE" "$OUT" "[dry-run] umount $T/firmware"
expect_contains "$CASE" "$OUT" "[dry-run] xbps-install -Syf rpi-kernel-6.12.95_1 rpi-firmware-1_1"
expect_absent "$CASE" "$STUB_LOG" "umount"
expect_absent "$CASE" "$STUB_LOG" "xbps-install -Syf"
[ ! -e "$T/backups/bootfs-pre-65.tar" ] && ok "$CASE: no backup written" || not_ok "$CASE: backup written"

# --- 4. a failure after the fstab change prints the rollback instructions ----

new_case rollback-message
RC=0
env PATH="$BIN:$PATH" STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" STUB_MOUNT_FAIL=1 \
    MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
    MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
    sh "$SCRIPT" --apply >"$OUT" 2>"$ERR" || RC=$?
expect_status "$CASE" nonzero
expect_contains "$CASE" "$ERR" "FAILED part-way"
expect_contains "$CASE" "$ERR" "Rollback:"

# --- 5. full apply: verification tolerates the modified conf_files -----------

new_case apply-ok
run_script --apply
expect_status "$CASE" zero
expect_contains "$CASE" "$OUT" "migration complete"
expect_contains "$CASE" "$OUT" "backup-6.12.87"
expect_contains "$CASE" "$OUT" "mount --bind / /mnt/r"
expect_contains "$CASE" "$STUB_LOG" "xbps-install -Syf rpi-kernel-6.12.95_1 rpi-firmware-1_1"

# --- 6. resume: /boot already vfat -> no fstab/remount, but re-extract + verify

new_case resume
RC=0
env PATH="$BIN:$PATH" STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" STUB_BOOT_VFAT=1 \
    MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
    MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
    sh "$SCRIPT" --apply >"$OUT" 2>"$ERR" || RC=$?
expect_status "$CASE" zero
expect_contains "$CASE" "$OUT" "resuming"
expect_contains "$CASE" "$STUB_LOG" "xbps-install -Syf rpi-kernel-6.12.95_1 rpi-firmware-1_1"
expect_contains "$CASE" "$STUB_LOG" "xbps-pkgdb rpi-kernel"
expect_absent "$CASE" "$STUB_LOG" "umount"
expect_absent "$CASE" "$STUB_LOG" "mount "

# --- 7. a dry run that shows an upgrade is refused before any change ----------

new_case xbps-dry-run-upgrades
RC=0
env PATH="$BIN:$PATH" STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" STUB_XBPS_UPGRADE=1 \
    MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
    MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
    sh "$SCRIPT" --apply >"$OUT" 2>"$ERR" || RC=$?
expect_status "$CASE" nonzero
expect_contains "$CASE" "$ERR" "does not show a plain reinstall"
expect_contains "$CASE" "$ERR" "Nothing was changed"
expect_absent "$CASE" "$STUB_LOG" "umount"
expect_absent "$CASE" "$STUB_LOG" "xbps-install -Syf"
[ ! -e "$T/backups/bootfs-pre-65.tar" ] && ok "$CASE: no backup written" || not_ok "$CASE: backup written"

# --- 8. the rollback text says how to restore p1 from the tar backup ----------

new_case rollback-restore-hint
RC=0
env PATH="$BIN:$PATH" STUB_LOG="$STUB_LOG" STUB_STATE="$T/state" STUB_MOUNT_FAIL=1 \
    MIGRATE_FSTAB="$T/fstab" MIGRATE_BOOT="$T/boot" MIGRATE_FIRMWARE="$T/firmware" \
    MIGRATE_BACKUP_DIR="$T/backups" MIGRATE_MODULES="$T/modules" \
    sh "$SCRIPT" --apply >"$OUT" 2>"$ERR" || RC=$?
expect_contains "$CASE" "$ERR" "tar -C $T/firmware -xf $T/backups/bootfs-pre-65.tar"

echo
echo "$PASSES passed, $FAILS failed"
[ "$FAILS" -eq 0 ]
