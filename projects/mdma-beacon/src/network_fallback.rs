//! Network fallback: on a wifi-only unit, reuse the wifi credentials of the
//! MDMA system installed on NVMe.
//!
//! At startup, when there is no ethernet link, the installed root partition is
//! mounted READ-ONLY, its `wpa_supplicant.conf` is copied verbatim into
//! `/run/beacon/wifi/` (verbatim keeps hand-added lines such as the `freq_list`
//! 5 GHz pin), the partition is unmounted, and the `beacon-wifi` runit service
//! is started. Every failure is logged and leaves the beacon exactly as it
//! was without this feature.
//!
//! Hard rules: only the root partition is ever mounted (never music, var or
//! metadata), always `ro,noload`, nothing is written under the mount, the
//! mount is always undone, and neither the file contents nor the psk reach the
//! log.

use crate::block_devices::{
    blkid_args, interpret_blkid, scan_nvme_drives, BlkidResult, OnDiskTable, ScannedDrive,
};
use crate::mounts::{parse_mounts, MountEntry};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};
use wifi_config::{
    FrequencyPin, InstalledWifiConf, InstalledWifiSummary, MAX_INSTALLED_CONF_BYTES,
    WIFI_INTERFACE, WPA_SUPPLICANT_CONF_PATH,
};

/// The runit service that runs wpa_supplicant on the copied configuration.
const WIFI_SERVICE: &str = "beacon-wifi";
/// GPT partition name (and ext4 label) of the installed root.
const ROOT_NAME: &str = "root";
const ETH_INTERFACE: &str = "eth0";
const ETH_CARRIER_PATH: &str = "/sys/class/net/eth0/carrier";
const MOUNT_DIR: &str = "/run/beacon/installed-root";
const WIFI_DIR: &str = "/run/beacon/wifi";
const WIFI_CONF_NAME: &str = "wpa_supplicant.conf";
const CARRIER_POLL: Duration = Duration::from_millis(500);
const CARRIER_TIMEOUT: Duration = Duration::from_secs(10);

// ── Outcome (for the UI) ──────────────────────────────────────────────────────

/// Where the beacon's network came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetworkFallback {
    /// The fallback was not attempted (check mode).
    Skipped,
    /// Ethernet has a link; the NVMe was not touched.
    Ethernet,
    NoWifiHardware,
    NoInstalledSystem,
    /// The installed system's wifi configuration is in use. The configuration
    /// is kept (its `Debug` is redacted) so a reprovisioning with blank wifi
    /// fields can carry it over verbatim.
    WifiFromInstalledSystem {
        conf: InstalledWifiConf,
    },
    /// Something went wrong; the reason never contains secrets.
    Failed(String),
}

/// The note appended to the network label for a `freq_list`.
pub(crate) fn frequency_pin_note(summary: &InstalledWifiSummary) -> &'static str {
    match summary.frequency_pin() {
        FrequencyPin::NotPinned => "",
        FrequencyPin::FiveGhz => " (5 GHz pinned)",
        FrequencyPin::Other => " (frequencies pinned)",
    }
}

impl NetworkFallback {
    /// One line for the main page. Never contains the psk.
    pub(crate) fn label(&self) -> String {
        match self {
            NetworkFallback::Ethernet => "Network: ethernet".to_string(),
            NetworkFallback::WifiFromInstalledSystem { conf } => {
                let summary = conf.summary();
                format!(
                    "Network: wifi from installed system \u{2014} SSID {}{}",
                    summary.ssids.join(", "),
                    frequency_pin_note(&summary)
                )
            }
            NetworkFallback::Skipped
            | NetworkFallback::NoWifiHardware
            | NetworkFallback::NoInstalledSystem
            | NetworkFallback::Failed(_) => "Network: none detected".to_string(),
        }
    }

    /// Whether the beacon found a network to use.
    pub(crate) fn has_network(&self) -> bool {
        matches!(
            self,
            NetworkFallback::Ethernet | NetworkFallback::WifiFromInstalledSystem { .. }
        )
    }

    /// Whether the network is the installed system's wifi.
    pub(crate) fn is_wifi_from_installed_system(&self) -> bool {
        matches!(self, NetworkFallback::WifiFromInstalledSystem { .. })
    }

    /// The installed system's wifi configuration, when that is the network.
    pub(crate) fn installed_wifi_conf(&self) -> Option<&InstalledWifiConf> {
        match self {
            NetworkFallback::WifiFromInstalledSystem { conf } => Some(conf),
            _ => None,
        }
    }

    /// The SSIDs of the installed system's wifi, comma separated, for the form
    /// note. `None` unless the network is the installed system's wifi. Never
    /// contains the psk.
    pub(crate) fn installed_wifi_ssids(&self) -> Option<String> {
        self.installed_wifi_conf()
            .map(|conf| conf.summary().ssids.join(", "))
    }
}

// ── Pure decisions ────────────────────────────────────────────────────────────

/// Whether the carrier file says there is a link. Only a trimmed "1" counts;
/// a read error (the interface is down: EINVAL) means no link.
pub(crate) fn eth_link(carrier: io::Result<String>) -> bool {
    matches!(carrier, Ok(text) if text.trim() == "1")
}

/// Poll `read` until there is a link or `timeout` has been waited out.
/// Returns as soon as there is a link. The time waited is the sum of the
/// sleeps, so the wait is deterministic under an injected `sleep`.
pub(crate) fn wait_for_eth_link(
    mut read: impl FnMut() -> io::Result<String>,
    mut sleep: impl FnMut(Duration),
    poll: Duration,
    timeout: Duration,
) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        if eth_link(read()) {
            return true;
        }
        if waited >= timeout {
            return false;
        }
        sleep(poll);
        waited += poll;
    }
}

/// The installed root partition, if there is exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootCandidate {
    None,
    One(String),
    /// More than one partition named `root`: do nothing.
    Ambiguous,
}

/// Find the GPT partition named `root` across all drives. Never goes by
/// partition number (p2 on 909/101 units, p1 on 303).
pub(crate) fn find_installed_root(drives: &[ScannedDrive]) -> RootCandidate {
    let mut roots = drives
        .iter()
        .filter_map(|d| match &d.table {
            OnDiskTable::Table { label, partitions } if label == "gpt" => Some(partitions),
            _ => None,
        })
        .flatten()
        .filter(|p| p.name.as_deref() == Some(ROOT_NAME))
        .map(|p| p.node.clone());

    match (roots.next(), roots.next()) {
        (None, _) => RootCandidate::None,
        (Some(node), None) => RootCandidate::One(node),
        (Some(_), Some(_)) => RootCandidate::Ambiguous,
    }
}

/// An ext4 filesystem labelled `root`.
pub(crate) fn root_is_ext4(blkid: &BlkidResult) -> bool {
    matches!(
        blkid,
        BlkidResult::Signature { fs_type, label }
            if fs_type == "ext4" && label.as_deref() == Some(ROOT_NAME)
    )
}

/// Arguments for `mount`. Always read-only, and `noload` so ext4 does not
/// replay its journal (which would write to the device). The disk's contents
/// are untrusted: `nosuid,nodev,noexec` so nothing on it can act as a program
/// or device node. A safety guard: do not loosen.
pub(crate) fn mount_args(dev: &str, dir: &Path) -> Vec<String> {
    vec![
        "-t".to_string(),
        "ext4".to_string(),
        "-o".to_string(),
        "ro,noload,nosuid,nodev,noexec".to_string(),
        dev.to_string(),
        dir.display().to_string(),
    ]
}

/// Where the installed root is mounted right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootMount {
    NotMounted,
    /// Mounted at exactly our mount directory and nowhere else: a leftover of an
    /// earlier run that was killed. Safe to unmount and continue.
    OurLeftover,
    /// Mounted somewhere else (possibly as well): not ours, leave it alone.
    Elsewhere,
}

/// Classify the mount state of `device` against the mount table.
pub(crate) fn classify_root_mount(
    mounts: &[MountEntry],
    device: &str,
    mount_dir: &Path,
) -> RootMount {
    let mut targets = mounts
        .iter()
        .filter(|m| m.device == device)
        .map(|m| m.target.as_path())
        .peekable();
    if targets.peek().is_none() {
        RootMount::NotMounted
    } else if targets.all(|target| target == mount_dir) {
        RootMount::OurLeftover
    } else {
        RootMount::Elsewhere
    }
}

// ── Reading the configuration from the mount ─────────────────────────────────

/// Open `path` for reading without following a symlink at the last component
/// (`O_NOFOLLOW`), then check on the open handle that it is a regular file. This
/// closes the window between the `symlink_metadata` walk and the open.
fn open_regular_file_nofollow(path: &Path) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        // O_NONBLOCK: opening a FIFO swapped in at the last moment must not hang.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Read the installed `wpa_supplicant.conf` below `root`. Every path component
/// must be a real directory and the file a regular file: a symlink (absolute
/// ones would resolve onto the beacon's own filesystem) is refused. The read is
/// capped. Errors never contain file contents.
fn read_installed_conf(root: &Path) -> Result<String, String> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = Path::new(WPA_SUPPLICANT_CONF_PATH).components().collect();
    for (i, component) in components.iter().enumerate() {
        current.push(component);
        let meta = std::fs::symlink_metadata(&current)
            .map_err(|e| format!("cannot stat {}: {e}", WPA_SUPPLICANT_CONF_PATH))?;
        let is_last = i + 1 == components.len();
        let ok = if is_last {
            meta.file_type().is_file()
        } else {
            meta.file_type().is_dir()
        };
        if !ok {
            return Err(format!(
                "{} on the installed system is not a regular file (symlinks are refused)",
                WPA_SUPPLICANT_CONF_PATH
            ));
        }
    }

    let file = open_regular_file_nofollow(&current)
        .map_err(|e| format!("cannot open {}: {e}", WPA_SUPPLICANT_CONF_PATH))?;
    let mut bytes = Vec::new();
    file.take(MAX_INSTALLED_CONF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", WPA_SUPPLICANT_CONF_PATH))?;
    if bytes.len() > MAX_INSTALLED_CONF_BYTES {
        return Err(format!(
            "{} is larger than {} bytes",
            WPA_SUPPLICANT_CONF_PATH, MAX_INSTALLED_CONF_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("{} is not valid UTF-8", WPA_SUPPLICANT_CONF_PATH))
}

// ── Side effects ──────────────────────────────────────────────────────────────

/// A finished external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// What the fallback needs from the machine. Faked in tests.
pub(crate) trait Host {
    fn sleep(&self, duration: Duration);
    fn scan_nvme_drives(&self) -> Result<Vec<ScannedDrive>, String>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CmdOutput>;
}

/// The real machine.
pub(crate) struct SystemHost;

impl Host for SystemHost {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn scan_nvme_drives(&self) -> Result<Vec<ScannedDrive>, String> {
        scan_nvme_drives(&[]).map_err(|e| e.to_string())
    }

    fn run(&self, program: &str, args: &[&str]) -> io::Result<CmdOutput> {
        let out = std::process::Command::new(program).args(args).output()?;
        Ok(CmdOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Where things are. Injectable so tests can use a temp directory.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub carrier_path: PathBuf,
    pub wlan_path: PathBuf,
    pub mount_dir: PathBuf,
    pub wifi_dir: PathBuf,
    pub carrier_poll: Duration,
    pub carrier_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            carrier_path: PathBuf::from(ETH_CARRIER_PATH),
            wlan_path: Path::new("/sys/class/net").join(WIFI_INTERFACE),
            mount_dir: PathBuf::from(MOUNT_DIR),
            wifi_dir: PathBuf::from(WIFI_DIR),
            carrier_poll: CARRIER_POLL,
            carrier_timeout: CARRIER_TIMEOUT,
        }
    }
}

/// Unmounts on drop, so every path out of the function (including errors and
/// early returns) undoes the mount.
struct MountGuard<'a> {
    host: &'a dyn Host,
    dir: &'a Path,
}

impl Drop for MountGuard<'_> {
    fn drop(&mut self) {
        let dir = self.dir.display().to_string();
        match self.host.run("umount", &[&dir]) {
            Ok(out) if out.success() => {}
            Ok(out) => warn!("umount {dir} failed: {}", out.stderr.trim()),
            Err(e) => warn!("could not run umount {dir}: {e}"),
        }
    }
}

/// Run `blkid` on `device` through the host.
fn probe(host: &dyn Host, device: &str) -> BlkidResult {
    match host.run("blkid", &blkid_args(device)) {
        Ok(out) => interpret_blkid(out.code, &out.stdout, &out.stderr),
        Err(e) => BlkidResult::Error(format!("failed to run blkid: {e}")),
    }
}

/// Write the configuration into the private wifi directory (0700, file 0600).
fn install_conf(dir: &Path, conf: &InstalledWifiConf) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    private_file::write_private_file(&dir.join(WIFI_CONF_NAME), conf.as_str().as_bytes())
}

fn failed(reason: impl Into<String>) -> NetworkFallback {
    let reason = reason.into();
    warn!("Network fallback: {reason}");
    NetworkFallback::Failed(reason)
}

/// Bring up wifi from the installed system's configuration when there is no
/// ethernet. Never fails: every problem is logged and reported as the outcome.
pub(crate) fn bring_up_wifi_from_installed_system(
    host: &dyn Host,
    settings: &Settings,
) -> NetworkFallback {
    // 1. Ethernet wins. A downed interface reports no carrier at all, so bring
    // it up first; the result is ignored (no eth0 simply means no link below).
    if let Err(e) = host.run("ip", &["link", "set", ETH_INTERFACE, "up"]) {
        info!("could not run ip link set {ETH_INTERFACE} up: {e}");
    }
    let has_link = wait_for_eth_link(
        || std::fs::read_to_string(&settings.carrier_path),
        |d| host.sleep(d),
        settings.carrier_poll,
        settings.carrier_timeout,
    );
    if has_link {
        info!("ethernet link; not touching NVMe");
        return NetworkFallback::Ethernet;
    }

    // 2. Wifi hardware.
    if !settings.wlan_path.exists() {
        info!(
            "No wifi hardware ({}); no fallback",
            settings.wlan_path.display()
        );
        return NetworkFallback::NoWifiHardware;
    }

    // 3. The installed root.
    let drives = match host.scan_nvme_drives() {
        Ok(drives) => drives,
        Err(reason) => return failed(format!("cannot scan NVMe drives: {reason}")),
    };
    let device = match find_installed_root(&drives) {
        RootCandidate::One(device) => device,
        RootCandidate::None => {
            info!("No installed system found on NVMe; no wifi fallback");
            return NetworkFallback::NoInstalledSystem;
        }
        RootCandidate::Ambiguous => {
            warn!("More than one partition named '{ROOT_NAME}' on NVMe; not touching any");
            return NetworkFallback::NoInstalledSystem;
        }
    };

    // 4. It must be an ext4 root, and not already mounted.
    let blkid = probe(host, &device);
    if !root_is_ext4(&blkid) {
        warn!(
            "{device} is not an ext4 filesystem labelled '{ROOT_NAME}' ({blkid:?}); not mounting"
        );
        return NetworkFallback::NoInstalledSystem;
    }
    let mounts = match host.run("mount", &[]) {
        Ok(out) if out.success() => parse_mounts(&out.stdout),
        Ok(out) => {
            return failed(format!(
                "cannot read the mount table: {}",
                out.stderr.trim()
            ))
        }
        Err(e) => return failed(format!("cannot read the mount table: {e}")),
    };
    match classify_root_mount(&mounts, &device, &settings.mount_dir) {
        RootMount::NotMounted => {}
        RootMount::Elsewhere => {
            warn!("{device} is already mounted elsewhere; not touching it");
            return NetworkFallback::NoInstalledSystem;
        }
        RootMount::OurLeftover => {
            // Our own mount from a run that was killed before it could undo it.
            let dir = settings.mount_dir.display().to_string();
            warn!("{device} is still mounted at {dir} (leftover); unmounting it");
            match host.run("umount", &[&dir]) {
                Ok(out) if out.success() => {}
                Ok(out) => return failed(format!("umount {dir} failed: {}", out.stderr.trim())),
                Err(e) => return failed(format!("could not run umount {dir}: {e}")),
            }
        }
    }

    // 5. Mount read-only.
    if let Err(e) = std::fs::create_dir_all(&settings.mount_dir) {
        return failed(format!(
            "cannot create {}: {e}",
            settings.mount_dir.display()
        ));
    }
    let args = mount_args(&device, &settings.mount_dir);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match host.run("mount", &args) {
        Ok(out) if out.success() => {}
        Ok(out) => return failed(format!("mounting {device} failed: {}", out.stderr.trim())),
        Err(e) => return failed(format!("could not run mount for {device}: {e}")),
    }

    // 6-7. Read the file; the guard unmounts when this block ends, whatever
    // happened inside it.
    let text = {
        let _unmount = MountGuard {
            host,
            dir: &settings.mount_dir,
        };
        read_installed_conf(&settings.mount_dir)
    };
    let text = match text {
        Ok(text) => text,
        Err(reason) => return failed(reason),
    };

    // 8. Validate and install.
    let Some(conf) = InstalledWifiConf::parse(&text) else {
        return failed(format!(
            "{WPA_SUPPLICANT_CONF_PATH} on the installed system has no usable network"
        ));
    };
    if let Err(e) = install_conf(&settings.wifi_dir, &conf) {
        return failed(format!("cannot write {}: {e}", settings.wifi_dir.display()));
    }

    // 9. Start wifi.
    match host.run("sv", &["up", WIFI_SERVICE]) {
        Ok(out) if out.success() => {}
        Ok(out) => {
            return failed(format!(
                "sv up {WIFI_SERVICE} failed: {}",
                out.stderr.trim()
            ))
        }
        Err(e) => return failed(format!("could not run sv up {WIFI_SERVICE}: {e}")),
    }

    // 10. Log what is safe to log.
    let summary = conf.summary();
    info!(
        "Wifi from installed system: SSIDs {:?}, country {:?}, freq_list kept: {}",
        summary.ssids, summary.country, summary.has_freq_list
    );
    NetworkFallback::WifiFromInstalledSystem { conf }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_devices::OnDiskPartition;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use std::cell::RefCell;

    fn err() -> io::Result<String> {
        Err(io::Error::from_raw_os_error(22))
    }

    // ---- eth_link ----

    #[rstest]
    #[case(Ok("1\n".to_string()), true)]
    #[case(Ok("1".to_string()), true)]
    #[case(Ok(" 1 \n".to_string()), true)]
    #[case(Ok("0\n".to_string()), false)]
    #[case(Ok(String::new()), false)]
    #[case(Ok("11".to_string()), false)]
    #[case(Ok("yes".to_string()), false)]
    #[case(err(), false)]
    fn eth_link_cases(#[case] carrier: io::Result<String>, #[case] expected: bool) {
        assert_eq!(eth_link(carrier), expected);
    }

    // ---- wait_for_eth_link ----

    const POLL: Duration = Duration::from_millis(500);

    #[test]
    fn link_present_returns_without_sleeping() {
        let mut sleeps = 0;
        let up = wait_for_eth_link(
            || Ok("1\n".to_string()),
            |_| sleeps += 1,
            POLL,
            Duration::from_secs(10),
        );
        assert!(up);
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn link_arriving_midway_returns_early() {
        let mut reads = 0;
        let mut slept = Duration::ZERO;
        let up = wait_for_eth_link(
            || {
                reads += 1;
                if reads < 4 {
                    err()
                } else {
                    Ok("1".to_string())
                }
            },
            |d| slept += d,
            POLL,
            Duration::from_secs(10),
        );
        assert!(up);
        assert_eq!(slept, POLL * 3);
    }

    #[test]
    fn no_link_waits_out_the_whole_timeout() {
        let mut slept = Duration::ZERO;
        let up = wait_for_eth_link(
            || Ok("0\n".to_string()),
            |d| slept += d,
            POLL,
            Duration::from_secs(10),
        );
        assert!(!up);
        assert_eq!(slept, Duration::from_secs(10));
    }

    #[test]
    fn zero_timeout_reads_once() {
        let mut reads = 0;
        let up = wait_for_eth_link(
            || {
                reads += 1;
                err()
            },
            |_| panic!("must not sleep"),
            POLL,
            Duration::ZERO,
        );
        assert!(!up);
        assert_eq!(reads, 1);
    }

    #[test]
    fn default_carrier_timeout_is_ten_seconds() {
        assert_eq!(Settings::default().carrier_timeout, Duration::from_secs(10));
    }

    // ---- find_installed_root ----

    fn part(node: &str, name: Option<&str>) -> OnDiskPartition {
        OnDiskPartition {
            node: node.to_string(),
            name: name.map(str::to_string),
            size_bytes: None,
        }
    }

    fn gpt(device: &str, parts: Vec<OnDiskPartition>) -> ScannedDrive {
        ScannedDrive {
            device: device.to_string(),
            table: OnDiskTable::Table {
                label: "gpt".to_string(),
                partitions: parts,
            },
        }
    }

    #[rstest]
    #[case::none(vec![], RootCandidate::None)]
    #[case::blank_drive(
        vec![ScannedDrive { device: "/dev/nvme0n1".into(), table: OnDiskTable::NoTable }],
        RootCandidate::None
    )]
    #[case::signature_only(
        vec![ScannedDrive {
            device: "/dev/nvme0n1".into(),
            table: OnDiskTable::Unrecognised { signature: "ext4".into() },
        }],
        RootCandidate::None
    )]
    #[case::dos_table_ignored(
        vec![ScannedDrive {
            device: "/dev/nvme0n1".into(),
            table: OnDiskTable::Table {
                label: "dos".into(),
                partitions: vec![part("/dev/nvme0n1p1", Some("root"))],
            },
        }],
        RootCandidate::None
    )]
    #[case::p2_on_909(
        vec![gpt("/dev/nvme0n1", vec![
            part("/dev/nvme0n1p1", Some("boot")),
            part("/dev/nvme0n1p2", Some("root")),
            part("/dev/nvme0n1p3", Some("var")),
        ])],
        RootCandidate::One("/dev/nvme0n1p2".into())
    )]
    #[case::p1_on_303(
        vec![gpt("/dev/nvme0n1", vec![
            part("/dev/nvme0n1p1", Some("root")),
            part("/dev/nvme0n1p2", Some("music")),
        ])],
        RootCandidate::One("/dev/nvme0n1p1".into())
    )]
    #[case::on_second_drive(
        vec![
            gpt("/dev/nvme0n1", vec![part("/dev/nvme0n1p1", Some("music"))]),
            gpt("/dev/nvme1n1", vec![part("/dev/nvme1n1p2", Some("root"))]),
        ],
        RootCandidate::One("/dev/nvme1n1p2".into())
    )]
    #[case::unnamed_and_other_names_ignored(
        vec![gpt("/dev/nvme0n1", vec![
            part("/dev/nvme0n1p1", None),
            part("/dev/nvme0n1p2", Some("rootfs")),
            part("/dev/nvme0n1p3", Some("Root")),
        ])],
        RootCandidate::None
    )]
    #[case::two_on_one_drive(
        vec![gpt("/dev/nvme0n1", vec![
            part("/dev/nvme0n1p1", Some("root")),
            part("/dev/nvme0n1p2", Some("root")),
        ])],
        RootCandidate::Ambiguous
    )]
    #[case::one_on_each_drive(
        vec![
            gpt("/dev/nvme0n1", vec![part("/dev/nvme0n1p2", Some("root"))]),
            gpt("/dev/nvme1n1", vec![part("/dev/nvme1n1p2", Some("root"))]),
        ],
        RootCandidate::Ambiguous
    )]
    fn find_installed_root_cases(
        #[case] drives: Vec<ScannedDrive>,
        #[case] expected: RootCandidate,
    ) {
        assert_eq!(find_installed_root(&drives), expected);
    }

    // ---- root_is_ext4 ----

    fn sig(fs: &str, label: Option<&str>) -> BlkidResult {
        BlkidResult::Signature {
            fs_type: fs.to_string(),
            label: label.map(str::to_string),
        }
    }

    #[rstest]
    #[case(sig("ext4", Some("root")), true)]
    #[case(sig("ext4", Some("music")), false)]
    #[case(sig("ext4", None), false)]
    #[case(sig("vfat", Some("root")), false)]
    #[case(sig("ext3", Some("root")), false)]
    #[case(BlkidResult::NoSignature, false)]
    #[case(BlkidResult::Error("boom".into()), false)]
    fn root_is_ext4_cases(#[case] blkid: BlkidResult, #[case] expected: bool) {
        assert_eq!(root_is_ext4(&blkid), expected);
    }

    // ---- mount_args ----

    #[test]
    fn mount_is_always_read_only_noload_ext4() {
        // Safety guard: these exact options must never change.
        assert_eq!(
            mount_args("/dev/nvme0n1p2", Path::new("/run/beacon/installed-root")),
            vec![
                "-t",
                "ext4",
                "-o",
                "ro,noload,nosuid,nodev,noexec",
                "/dev/nvme0n1p2",
                "/run/beacon/installed-root"
            ]
        );
    }

    // ---- classify_root_mount ----

    fn entry(device: &str, target: &str) -> MountEntry {
        MountEntry {
            device: device.into(),
            target: target.into(),
        }
    }

    #[rstest]
    #[case::not_mounted(vec![entry("/dev/mmcblk0p2", "/")], RootMount::NotMounted)]
    #[case::empty(vec![], RootMount::NotMounted)]
    #[case::leftover(vec![entry(ROOT_DEV, "/run/beacon/installed-root")], RootMount::OurLeftover)]
    #[case::elsewhere(vec![entry(ROOT_DEV, "/mnt/mdma-install")], RootMount::Elsewhere)]
    #[case::sub_directory_is_elsewhere(
        vec![entry(ROOT_DEV, "/run/beacon/installed-root/sub")],
        RootMount::Elsewhere
    )]
    #[case::both(
        vec![
            entry(ROOT_DEV, "/run/beacon/installed-root"),
            entry(ROOT_DEV, "/mnt/mdma-install"),
        ],
        RootMount::Elsewhere
    )]
    #[case::other_device_at_our_dir(
        vec![entry("/dev/nvme0n1p3", "/run/beacon/installed-root")],
        RootMount::NotMounted
    )]
    fn classify_root_mount_cases(#[case] mounts: Vec<MountEntry>, #[case] expected: RootMount) {
        assert_eq!(
            classify_root_mount(&mounts, ROOT_DEV, Path::new("/run/beacon/installed-root")),
            expected
        );
    }

    // ---- open_regular_file_nofollow ----

    #[test]
    fn nofollow_open_reads_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, "x").unwrap();
        let mut text = String::new();
        open_regular_file_nofollow(&path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "x");
    }

    #[test]
    fn nofollow_open_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_regular_file_nofollow(&link).is_err());
    }

    #[test]
    fn nofollow_open_refuses_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_regular_file_nofollow(dir.path()).is_err());
    }

    // ---- outcome labels ----

    fn conf_with(ssids: &[&str], freq_list: Option<&str>) -> InstalledWifiConf {
        let mut text = String::from("country=SE\n");
        for ssid in ssids {
            text.push_str(&format!(
                "network={{\n    ssid=\"{ssid}\"\n    psk=\"hunter2-secret\"\n"
            ));
            if let Some(freqs) = freq_list {
                text.push_str(&format!("    freq_list={freqs}\n"));
            }
            text.push_str("}\n");
        }
        InstalledWifiConf::parse(&text).expect("usable conf")
    }

    fn wifi(ssids: &[&str], freq_list: Option<&str>) -> NetworkFallback {
        NetworkFallback::WifiFromInstalledSystem {
            conf: conf_with(ssids, freq_list),
        }
    }

    #[rstest]
    #[case(NetworkFallback::Ethernet, "Network: ethernet")]
    #[case(NetworkFallback::Skipped, "Network: none detected")]
    #[case(NetworkFallback::NoWifiHardware, "Network: none detected")]
    #[case(NetworkFallback::NoInstalledSystem, "Network: none detected")]
    #[case(NetworkFallback::Failed("x".into()), "Network: none detected")]
    #[case(
        wifi(&["Home"], Some("5180 5200")),
        "Network: wifi from installed system \u{2014} SSID Home (5 GHz pinned)"
    )]
    #[case::two_four_ghz_pin(
        wifi(&["Home"], Some("2412")),
        "Network: wifi from installed system \u{2014} SSID Home (frequencies pinned)"
    )]
    #[case::mixed_pin(
        wifi(&["Home"], Some("2412 5180")),
        "Network: wifi from installed system \u{2014} SSID Home (frequencies pinned)"
    )]
    #[case(
        wifi(&["Home"], None),
        "Network: wifi from installed system \u{2014} SSID Home"
    )]
    #[case(
        wifi(&["A", "B"], None),
        "Network: wifi from installed system \u{2014} SSID A, B"
    )]
    fn labels(#[case] outcome: NetworkFallback, #[case] expected: &str) {
        assert_eq!(outcome.label(), expected);
    }

    #[test]
    fn label_never_contains_the_psk() {
        assert!(!wifi(&["Home"], Some("5180")).label().contains(SECRET));
        assert!(!format!("{:?}", wifi(&["Home"], None)).contains(SECRET));
    }

    #[test]
    fn ethernet_and_wifi_count_as_network() {
        assert!(NetworkFallback::Ethernet.has_network());
        assert!(wifi(&["x"], None).has_network());
        assert!(!NetworkFallback::NoInstalledSystem.has_network());
        assert!(!NetworkFallback::Failed("x".into()).has_network());
    }

    #[test]
    fn only_wifi_outcome_is_wifi() {
        assert!(wifi(&["x"], None).is_wifi_from_installed_system());
        assert!(!NetworkFallback::Ethernet.is_wifi_from_installed_system());
        assert!(!NetworkFallback::Failed("x".into()).is_wifi_from_installed_system());
    }

    #[test]
    fn only_wifi_outcome_has_a_conf_and_ssids_to_carry_over() {
        let outcome = wifi(&["A", "B"], Some("5180"));
        assert_eq!(
            outcome.installed_wifi_conf(),
            Some(&conf_with(&["A", "B"], Some("5180")))
        );
        assert_eq!(outcome.installed_wifi_ssids().as_deref(), Some("A, B"));
        for other in [
            NetworkFallback::Ethernet,
            NetworkFallback::Skipped,
            NetworkFallback::Failed("x".into()),
        ] {
            assert_eq!(other.installed_wifi_conf(), None);
            assert_eq!(other.installed_wifi_ssids(), None);
        }
    }

    // ---- the side-effect path, with a fake host ----

    const GOOD_CONF: &str = "country=SE\nctrl_interface=/run/wpa_supplicant\n\
        network={\n    ssid=\"HomeNet\"\n    psk=\"hunter2-secret\"\n    freq_list=5180 5200\n}\n";
    const SECRET: &str = "hunter2-secret";
    const ROOT_DEV: &str = "/dev/nvme0n1p2";

    /// Builds the fake installed root inside the mount directory.
    type Fixture = Box<dyn Fn(&Path)>;

    struct FakeHost {
        drives: Result<Vec<ScannedDrive>, String>,
        blkid: CmdOutput,
        mount_table: String,
        mount_ok: bool,
        umount_ok: bool,
        sv_ok: bool,
        /// `ip link set eth0 up`: `None` makes the command itself fail to run.
        ip_ok: Option<bool>,
        fixture: Fixture,
        calls: RefCell<Vec<String>>,
    }

    fn out(code: i32, stdout: &str) -> CmdOutput {
        CmdOutput {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn write_conf(root: &Path, text: &str) {
        let dir = root.join("etc/wpa_supplicant");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("wpa_supplicant.conf"), text).unwrap();
    }

    impl FakeHost {
        fn healthy() -> Self {
            FakeHost {
                drives: Ok(vec![gpt(
                    "/dev/nvme0n1",
                    vec![
                        part("/dev/nvme0n1p1", Some("boot")),
                        part(ROOT_DEV, Some("root")),
                    ],
                )]),
                blkid: out(0, "TYPE=ext4\nLABEL=root\n"),
                mount_table: "/dev/mmcblk0p2 on / type ext4 (rw)\n".to_string(),
                mount_ok: true,
                umount_ok: true,
                sv_ok: true,
                ip_ok: Some(true),
                fixture: Box::new(|root| write_conf(root, GOOD_CONF)),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn called(&self, program: &str) -> usize {
            self.calls()
                .iter()
                .filter(|c| c.split(' ').next() == Some(program))
                .count()
        }
    }

    impl Host for FakeHost {
        fn sleep(&self, _duration: Duration) {}

        fn scan_nvme_drives(&self) -> Result<Vec<ScannedDrive>, String> {
            self.drives.clone()
        }

        fn run(&self, program: &str, args: &[&str]) -> io::Result<CmdOutput> {
            self.calls.borrow_mut().push(
                format!("{program} {}", args.join(" "))
                    .trim_end()
                    .to_string(),
            );
            Ok(match (program, args) {
                ("ip", _) => match self.ip_ok {
                    Some(ok) => out(if ok { 0 } else { 2 }, ""),
                    None => return Err(io::Error::from(io::ErrorKind::NotFound)),
                },
                ("blkid", _) => self.blkid.clone(),
                ("mount", []) => out(0, &self.mount_table),
                ("mount", args) => {
                    if self.mount_ok {
                        (self.fixture)(Path::new(args[args.len() - 1]));
                        out(0, "")
                    } else {
                        out(32, "")
                    }
                }
                ("umount", _) => out(if self.umount_ok { 0 } else { 1 }, ""),
                ("sv", _) => out(if self.sv_ok { 0 } else { 1 }, ""),
                other => panic!("unexpected command {other:?}"),
            })
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        settings: Settings,
    }

    /// `carrier`: contents of the carrier file; `None` leaves it missing
    /// (reading fails, like a downed interface).
    fn env(carrier: Option<&str>, with_wlan: bool) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if let Some(text) = carrier {
            std::fs::write(root.join("carrier"), text).unwrap();
        }
        if with_wlan {
            std::fs::create_dir(root.join("wlan0")).unwrap();
        }
        let settings = Settings {
            carrier_path: root.join("carrier"),
            wlan_path: root.join("wlan0"),
            mount_dir: root.join("run/installed-root"),
            wifi_dir: root.join("run/wifi"),
            carrier_poll: Duration::from_millis(1),
            carrier_timeout: Duration::from_millis(3),
        };
        Env {
            _dir: dir,
            settings,
        }
    }

    fn run_fallback(host: &FakeHost, env: &Env) -> NetworkFallback {
        bring_up_wifi_from_installed_system(host, &env.settings)
    }

    #[test]
    fn ethernet_link_never_touches_nvme() {
        let host = FakeHost::healthy();
        let env = env(Some("1\n"), true);
        assert_eq!(run_fallback(&host, &env), NetworkFallback::Ethernet);
        assert_eq!(host.calls(), vec![IP_UP.to_string()]);
    }

    const IP_UP: &str = "ip link set eth0 up";

    #[rstest]
    #[case::ip_fails(Some(false))]
    #[case::ip_missing(None)]
    fn ip_link_up_result_is_ignored(#[case] ip_ok: Option<bool>) {
        let host = FakeHost {
            ip_ok,
            ..FakeHost::healthy()
        };
        let env = env(Some("1\n"), true);
        assert_eq!(run_fallback(&host, &env), NetworkFallback::Ethernet);
        assert_eq!(host.calls(), vec![IP_UP.to_string()]);
    }

    #[test]
    fn interface_is_brought_up_before_the_carrier_is_read() {
        // The carrier file only appears once the interface is up: the fake
        // writes it when it sees the `ip` call.
        struct UpWritesCarrier<'a>(&'a Path, FakeHost);
        impl Host for UpWritesCarrier<'_> {
            fn sleep(&self, d: Duration) {
                self.1.sleep(d)
            }
            fn scan_nvme_drives(&self) -> Result<Vec<ScannedDrive>, String> {
                self.1.scan_nvme_drives()
            }
            fn run(&self, program: &str, args: &[&str]) -> io::Result<CmdOutput> {
                if program == "ip" {
                    std::fs::write(self.0, "1\n").unwrap();
                }
                self.1.run(program, args)
            }
        }
        let env = env(None, true);
        let host = UpWritesCarrier(&env.settings.carrier_path, FakeHost::healthy());
        assert_eq!(
            bring_up_wifi_from_installed_system(&host, &env.settings),
            NetworkFallback::Ethernet
        );
    }

    #[test]
    fn missing_wifi_hardware_is_reported() {
        let host = FakeHost::healthy();
        let env = env(None, false);
        assert_eq!(run_fallback(&host, &env), NetworkFallback::NoWifiHardware);
        assert_eq!(host.calls(), vec![IP_UP.to_string()]);
    }

    #[rstest]
    #[case::no_root(Ok(vec![]))]
    #[case::ambiguous(Ok(vec![
        gpt("/dev/nvme0n1", vec![part("/dev/nvme0n1p2", Some("root"))]),
        gpt("/dev/nvme1n1", vec![part("/dev/nvme1n1p2", Some("root"))]),
    ]))]
    fn no_single_root_means_no_installed_system(#[case] drives: Result<Vec<ScannedDrive>, String>) {
        let host = FakeHost {
            drives,
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert_eq!(
            run_fallback(&host, &env),
            NetworkFallback::NoInstalledSystem
        );
        assert_eq!(host.called("mount"), 0);
    }

    #[test]
    fn scan_failure_is_a_failure_and_mounts_nothing() {
        let host = FakeHost {
            drives: Err("sfdisk exploded".into()),
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert!(matches!(
            run_fallback(&host, &env),
            NetworkFallback::Failed(_)
        ));
        assert_eq!(host.called("mount"), 0);
    }

    #[rstest]
    #[case::wrong_label(out(0, "TYPE=ext4\nLABEL=music\n"))]
    #[case::wrong_fs(out(0, "TYPE=vfat\nLABEL=root\n"))]
    #[case::no_signature(out(2, ""))]
    #[case::blkid_error(out(1, ""))]
    fn not_an_ext4_root_is_not_mounted(#[case] blkid: CmdOutput) {
        let host = FakeHost {
            blkid,
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert_eq!(
            run_fallback(&host, &env),
            NetworkFallback::NoInstalledSystem
        );
        assert_eq!(host.called("mount"), 0);
        assert_eq!(host.called("umount"), 0);
    }

    #[test]
    fn already_mounted_root_is_left_alone() {
        let host = FakeHost {
            mount_table: format!("{ROOT_DEV} on /mnt/mdma-install type ext4 (rw)\n"),
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert_eq!(
            run_fallback(&host, &env),
            NetworkFallback::NoInstalledSystem
        );
        assert_eq!(host.called("umount"), 0);
        assert_eq!(host.called("sv"), 0);
    }

    #[test]
    fn leftover_mount_of_ours_is_unmounted_and_the_run_continues() {
        let env = env(None, true);
        let mount_dir = env.settings.mount_dir.display().to_string();
        let host = FakeHost {
            mount_table: format!("{ROOT_DEV} on {mount_dir} type ext4 (ro,noload)\n"),
            ..FakeHost::healthy()
        };
        let outcome = run_fallback(&host, &env);
        assert!(
            matches!(outcome, NetworkFallback::WifiFromInstalledSystem { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            host.calls(),
            vec![
                IP_UP.to_string(),
                format!("blkid -p -o export -s TYPE -s LABEL {ROOT_DEV}"),
                "mount".to_string(),
                format!("umount {mount_dir}"),
                format!("mount -t ext4 -o ro,noload,nosuid,nodev,noexec {ROOT_DEV} {mount_dir}"),
                format!("umount {mount_dir}"),
                "sv up beacon-wifi".to_string(),
            ]
        );
    }

    #[test]
    fn failing_to_unmount_a_leftover_is_a_failure_and_mounts_nothing() {
        let env = env(None, true);
        let mount_dir = env.settings.mount_dir.display().to_string();
        let host = FakeHost {
            mount_table: format!("{ROOT_DEV} on {mount_dir} type ext4 (ro,noload)\n"),
            umount_ok: false,
            ..FakeHost::healthy()
        };
        assert!(matches!(
            run_fallback(&host, &env),
            NetworkFallback::Failed(_)
        ));
        assert_eq!(host.called("umount"), 1);
        assert_eq!(host.called("mount"), 1, "only the mount table read");
        assert_eq!(host.called("sv"), 0);
    }

    #[test]
    fn root_mounted_both_here_and_elsewhere_is_left_alone() {
        let env = env(None, true);
        let mount_dir = env.settings.mount_dir.display().to_string();
        let host = FakeHost {
            mount_table: format!(
                "{ROOT_DEV} on {mount_dir} type ext4 (ro)\n\
                 {ROOT_DEV} on /mnt/mdma-install type ext4 (rw)\n"
            ),
            ..FakeHost::healthy()
        };
        assert_eq!(
            run_fallback(&host, &env),
            NetworkFallback::NoInstalledSystem
        );
        assert_eq!(host.called("umount"), 0);
    }

    #[test]
    fn mount_failure_is_a_failure_without_unmount() {
        let host = FakeHost {
            mount_ok: false,
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert!(matches!(
            run_fallback(&host, &env),
            NetworkFallback::Failed(_)
        ));
        assert_eq!(host.called("umount"), 0);
        assert_eq!(host.called("sv"), 0);
    }

    #[test]
    fn happy_path_copies_verbatim_privately_and_starts_wifi() {
        use std::os::unix::fs::PermissionsExt;
        let host = FakeHost::healthy();
        let env = env(Some("0\n"), true);

        let outcome = run_fallback(&host, &env);

        assert_eq!(
            outcome,
            NetworkFallback::WifiFromInstalledSystem {
                conf: InstalledWifiConf::parse(GOOD_CONF).unwrap()
            }
        );
        assert_eq!(
            outcome.installed_wifi_conf().unwrap().summary(),
            InstalledWifiSummary {
                ssids: vec!["HomeNet".into()],
                country: Some("SE".into()),
                has_freq_list: true,
                freq_list: vec!["5180".into(), "5200".into()],
            }
        );
        let conf = env.settings.wifi_dir.join("wpa_supplicant.conf");
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), GOOD_CONF);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&env.settings.wifi_dir), 0o700);
        assert_eq!(mode(&conf), 0o600);

        let mount_dir = env.settings.mount_dir.display().to_string();
        assert_eq!(
            host.calls(),
            vec![
                IP_UP.to_string(),
                format!("blkid -p -o export -s TYPE -s LABEL {ROOT_DEV}"),
                "mount".to_string(),
                format!("mount -t ext4 -o ro,noload,nosuid,nodev,noexec {ROOT_DEV} {mount_dir}"),
                format!("umount {mount_dir}"),
                "sv up beacon-wifi".to_string(),
            ]
        );
        assert!(!format!("{outcome:?}").contains(SECRET));
    }

    #[test]
    fn unmount_failure_is_tolerated() {
        let host = FakeHost {
            umount_ok: false,
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        assert!(matches!(
            run_fallback(&host, &env),
            NetworkFallback::WifiFromInstalledSystem { .. }
        ));
        assert_eq!(host.called("umount"), 1);
    }

    /// Every failure after the mount must still unmount, and write nothing.
    fn assert_failed_but_unmounted(host: &FakeHost, env: &Env) {
        let outcome = run_fallback(host, env);
        let NetworkFallback::Failed(reason) = outcome else {
            panic!("expected Failed, got {outcome:?}");
        };
        assert!(!reason.contains(SECRET), "reason leaks: {reason}");
        assert_eq!(host.called("umount"), 1, "must unmount");
        assert_eq!(host.called("sv"), 0, "must not start wifi");
        assert!(!env.settings.wifi_dir.join("wpa_supplicant.conf").exists());
    }

    #[test]
    fn missing_conf_fails_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|_| {}),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn symlinked_conf_is_refused_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|root| {
                let dir = root.join("etc/wpa_supplicant");
                std::fs::create_dir_all(&dir).unwrap();
                // Points at a real, valid conf on the beacon's own filesystem.
                let target = root.join("elsewhere.conf");
                std::fs::write(&target, GOOD_CONF).unwrap();
                std::os::unix::fs::symlink(&target, dir.join("wpa_supplicant.conf")).unwrap();
            }),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn symlinked_directory_is_refused_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|root| {
                let real = root.join("real-etc");
                write_conf(&real, GOOD_CONF);
                std::os::unix::fs::symlink(&real, root.join("etc")).unwrap();
            }),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn directory_in_place_of_conf_is_refused_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|root| {
                std::fs::create_dir_all(root.join("etc/wpa_supplicant/wpa_supplicant.conf"))
                    .unwrap();
            }),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn oversized_conf_is_refused_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|root| {
                let big = format!("{GOOD_CONF}{}", "#".repeat(MAX_INSTALLED_CONF_BYTES));
                write_conf(root, &big);
            }),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[rstest]
    #[case::no_network("country=SE\n")]
    #[case::no_psk("network={\n ssid=\"x\"\n}\n")]
    fn unusable_conf_fails_and_unmounts(#[case] text: &'static str) {
        let host = FakeHost {
            fixture: Box::new(move |root| write_conf(root, text)),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn non_utf8_conf_fails_and_unmounts() {
        let host = FakeHost {
            fixture: Box::new(|root| {
                let dir = root.join("etc/wpa_supplicant");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("wpa_supplicant.conf"), [0xff, 0xfe, 0x00]).unwrap();
            }),
            ..FakeHost::healthy()
        };
        assert_failed_but_unmounted(&host, &env(None, true));
    }

    #[test]
    fn sv_failure_is_reported_without_secrets() {
        let host = FakeHost {
            sv_ok: false,
            ..FakeHost::healthy()
        };
        let env = env(None, true);
        let outcome = run_fallback(&host, &env);
        let NetworkFallback::Failed(reason) = outcome else {
            panic!("expected Failed, got {outcome:?}");
        };
        assert!(reason.contains("sv up"), "{reason}");
        assert!(!reason.contains(SECRET));
        assert_eq!(host.called("umount"), 1);
    }
}
