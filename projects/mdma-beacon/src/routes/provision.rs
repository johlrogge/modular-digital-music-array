use crate::hardware::HardwareInfo;
use crate::provisioning;
use crate::provisioning::types::{ProvisionConfig, UnitType};
use crate::provisioning::{ExistingDrive, LayoutError};
use crate::server::AppState;
use crate::types::{ExistingDataPolicy, Hostname, SshPublicKey};
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html,
    },
    Form,
};
use futures::stream::Stream;
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tracing::info;
use wifi_config::{CountryCode, NetworkConfig, WifiPassphrase, WifiSsid};

use super::{spawn_with_start_signal, AppError};

/// Main template for the welcome page
#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    hardware: HardwareInfo,
    version: String,
    build_time: &'static str,
    /// Present when stage 2 refused the existing disk layout. Nothing was
    /// changed. The page explains why and, where a wipe could resolve it,
    /// offers the deliberate (typed-confirmation) override.
    layout_error: Option<LayoutErrorContext>,
    /// What is on the NVMe drives right now: shown in the wipe confirmation.
    existing_drives: Vec<ExistingDrive>,
    /// Set when the drives could not be read (the wipe would abort too).
    existing_drives_error: Option<String>,
    /// The word the user must type to enable a wipe.
    wipe_word: &'static str,
    /// Identifies the drive list shown above; posted back with the wipe so the
    /// server can tell whether the drives changed while the page was open.
    /// Empty when the drives could not be read.
    wipe_fingerprint: String,
    /// A message shown at the top of the page (nothing was changed).
    notice: Option<String>,
}

/// Shown when the drives differ from what the user confirmed.
const DRIVES_CHANGED_NOTICE: &str = "The drives changed since this page was loaded. Nothing was changed; review the list and confirm again.";

/// Context passed to the template when stage 2 refuses the disk layout.
#[derive(Debug, Clone)]
struct LayoutErrorContext {
    /// Human-readable error from stage 2 (escaped by the template).
    message: String,
    /// Whether a deliberate wipe could resolve this. False when the drive could
    /// not be read or music/metadata sit on a drive the plan does not touch.
    can_wipe: bool,
    /// The submitted form, re-posted by the wipe override.
    unit_type: String,
    hostname: String,
    ssh_key: String,
    /// Wifi network name and country, re-posted by the override. The
    /// passphrase is deliberately absent: it is never echoed back into a page.
    wifi_ssid: String,
    wifi_country: String,
}

/// The word that must be typed to authorise `WipeAll`.
pub(crate) const WIPE_CONFIRMATION: &str = "WIPE";

/// Whether a wipe of the target drive(s) could resolve this refusal. It cannot
/// when the disk could not be read, or when data sits on a drive the plan does
/// not repartition (the enumeration-swap protection is never a user choice).
fn wipe_can_resolve(err: &LayoutError) -> bool {
    matches!(
        err,
        LayoutError::NotGpt { .. }
            | LayoutError::Mismatch { .. }
            | LayoutError::Unrecognised { .. }
    )
}

/// Validate the chosen policy. `WipeAll` is accepted only when the
/// confirmation word was typed exactly; a wipe choice with a missing or wrong
/// confirmation is an error, never a downgrade, so the user is not surprised
/// by a different outcome. Other choices ignore the confirmation.
fn resolve_existing_data(
    chosen: ExistingDataPolicy,
    confirmation: &str,
) -> Result<ExistingDataPolicy, String> {
    if chosen == ExistingDataPolicy::WipeAll && confirmation.trim() != WIPE_CONFIRMATION {
        return Err(format!(
            "Wiping the drive requires typing {} to confirm. Nothing was changed.",
            WIPE_CONFIRMATION
        ));
    }
    Ok(chosen)
}

/// Turn the submitted wifi fields into a network config. The SSID and country
/// are trimmed, so SSIDs with leading or trailing spaces are not supported; the
/// passphrase is not trimmed (spaces are valid in it). All three blank
/// means no wifi. There is no default country: the regulatory domain is the
/// user's to state.
fn resolve_network(ssid: &str, psk: &str, country: &str) -> Result<Option<NetworkConfig>, String> {
    let ssid = ssid.trim();
    let country = country.trim();

    if ssid.is_empty() && psk.is_empty() && country.is_empty() {
        return Ok(None);
    }
    if ssid.is_empty() {
        return Err(
            "Wifi settings were given without a network name (SSID). Nothing was changed."
                .to_string(),
        );
    }
    if psk.is_empty() {
        return Err(
            "A wifi network name (SSID) was given without a password. Nothing was changed."
                .to_string(),
        );
    }
    if country.is_empty() {
        return Err(
            "Wifi needs a two-letter country code (for example SE). Nothing was changed."
                .to_string(),
        );
    }

    let ssid = WifiSsid::new(ssid)
        .map_err(|e| format!("Invalid wifi network name: {e}. Nothing was changed."))?;
    let passphrase = WifiPassphrase::new(psk)
        .map_err(|e| format!("Invalid wifi password: {e}. Nothing was changed."))?;
    let country = CountryCode::new(country)
        .map_err(|e| format!("Invalid wifi country: {e}. Nothing was changed."))?;

    Ok(Some(NetworkConfig::new(ssid, &passphrase, country)))
}

/// The devices provisioning writes to. Stage 1 requires exactly the NVMe drives
/// the hardware reports (1-2 for the 909, exactly 1 otherwise), so every one of
/// them is a target; other NVMe devices that may appear are never wiped.
fn target_devices(hardware: &HardwareInfo) -> Vec<String> {
    hardware
        .nvme_drives
        .iter()
        .map(|d| d.device.as_str().to_string())
        .collect()
}

/// Keep only the drives a wipe could touch.
fn limit_to_targets(drives: Vec<ExistingDrive>, targets: &[String]) -> Vec<ExistingDrive> {
    drives
        .into_iter()
        .filter(|d| targets.contains(&d.device))
        .collect()
}

/// A deterministic fingerprint of what the drives hold (device paths and
/// partition descriptions, including whole-disk signatures), independent of
/// the order they were listed in. Stable within a beacon run; it is posted
/// back with the wipe confirmation so a swapped drive is detected.
fn drives_fingerprint(drives: &[ExistingDrive]) -> String {
    use std::hash::{Hash, Hasher};

    let mut normalised: Vec<(&str, Vec<&str>)> = drives
        .iter()
        .map(|d| {
            let mut partitions: Vec<&str> = d.partitions.iter().map(String::as_str).collect();
            partitions.sort_unstable();
            (d.device.as_str(), partitions)
        })
        .collect();
    normalised.sort_unstable();

    // `DefaultHasher::new()` uses fixed keys, so this is deterministic.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalised.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Whether the posted fingerprint matches the drives as they are now. `None`
/// (the scan failed) never matches, and neither does a missing fingerprint.
fn wipe_fingerprint_confirmed(submitted: &str, current: Option<&[ExistingDrive]>) -> bool {
    match current {
        Some(drives) => !submitted.is_empty() && submitted == drives_fingerprint(drives),
        None => false,
    }
}

/// Read the target drives for the confirmation panel without blocking the
/// runtime. Returns the drives and, if the scan failed, the reason.
async fn load_existing_drives(targets: &[String]) -> (Vec<ExistingDrive>, Option<String>) {
    match tokio::task::spawn_blocking(provisioning::describe_existing_drives).await {
        Ok(Ok(drives)) => (limit_to_targets(drives, targets), None),
        Ok(Err(reason)) => (Vec::new(), Some(reason)),
        Err(e) => (Vec::new(), Some(format!("drive scan failed: {}", e))),
    }
}

/// The fingerprint to embed in the page: empty when the scan failed, so the
/// wipe is refused later.
fn page_fingerprint(drives: &[ExistingDrive], error: &Option<String>) -> String {
    if error.is_some() {
        String::new()
    } else {
        drives_fingerprint(drives)
    }
}

/// Template for the provisioning progress page
#[derive(Template)]
#[template(path = "provision.html")]
struct ProvisionPageTemplate {
    is_dry_run: bool,
}

/// Provisioning form submission
#[derive(Deserialize)]
pub(crate) struct ProvisionForm {
    unit_type: String,
    hostname: String,
    ssh_key: String,
    /// Radio group, one exclusive choice: `preserve` (default), `reinstall_os`
    /// (reformat boot, root and var; music and metadata are never touched) or
    /// `wipe_all` (destroys ALL data including music and metadata; honoured
    /// only together with `wipe_confirmation`).
    #[serde(default)]
    existing_data: ExistingDataPolicy,
    /// Must equal [`WIPE_CONFIRMATION`] for `wipe_all` to take effect.
    #[serde(default)]
    wipe_confirmation: String,
    /// Fingerprint of the drive list the user saw; a wipe is refused unless it
    /// matches the drives as they are when the form is posted.
    #[serde(default)]
    wipe_fingerprint: String,
    /// Optional wifi network name.
    #[serde(default)]
    wifi_ssid: String,
    /// Wifi passphrase. Secret: never logged (see the manual `Debug`) and never
    /// rendered back into a page.
    #[serde(default)]
    wifi_psk: String,
    /// Two-letter regulatory country code for wifi.
    #[serde(default)]
    wifi_country: String,
}

// Hand-written so the passphrase can never reach the log (shown at /logs).
impl std::fmt::Debug for ProvisionForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let psk = if self.wifi_psk.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        f.debug_struct("ProvisionForm")
            .field("unit_type", &self.unit_type)
            .field("hostname", &self.hostname)
            .field("ssh_key", &self.ssh_key)
            .field("existing_data", &self.existing_data)
            .field("wipe_confirmation", &self.wipe_confirmation)
            .field("wipe_fingerprint", &self.wipe_fingerprint)
            .field("wifi_ssid", &self.wifi_ssid)
            .field("wifi_psk", &format_args!("{psk}"))
            .field("wifi_country", &self.wifi_country)
            .finish()
    }
}

/// Handler for the main page
pub async fn index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let hardware = state.hardware.lock().await.clone();
    let (existing_drives, existing_drives_error) =
        load_existing_drives(&target_devices(&hardware)).await;
    let template = IndexTemplate {
        hardware,
        version: crate::update::full_version(),
        build_time: crate::update::build_timestamp(),
        layout_error: None,
        wipe_fingerprint: page_fingerprint(&existing_drives, &existing_drives_error),
        existing_drives,
        existing_drives_error,
        wipe_word: WIPE_CONFIRMATION,
        notice: None,
    };

    let html = template
        .render()
        .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}

/// TEST endpoint - Simple SSE stream that sends messages every second
pub async fn test_stream() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        for i in 1..=10 {
            yield Ok(Event::default().data(format!("Test message {}", i)));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        yield Ok(Event::default().data("Stream complete!"));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// SSE endpoint for streaming provisioning logs — tails the on-disk log file.
pub async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = crate::log_tail::follow(state.log_path.clone(), 5000)
        .map(|line| Ok::<Event, Infallible>(Event::default().data(line)));

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Handler for provisioning request
pub async fn provision(
    State(state): State<AppState>,
    Form(form): Form<ProvisionForm>,
) -> Result<Html<String>, AppError> {
    info!("Received provisioning request: {:?}", form);

    // Parse and validate inputs using newtype constructors
    let unit_type = form.unit_type.parse::<UnitType>().map_err(AppError::from)?;
    let hostname = Hostname::new(form.hostname.clone())?;
    let ssh_key = SshPublicKey::new(form.ssh_key.clone())?;

    let network = resolve_network(&form.wifi_ssid, &form.wifi_psk, &form.wifi_country)
        .map_err(AppError::Validation)?;

    let config = ProvisionConfig {
        network,
        ssh_key,
        unit_type,
        hostname,
        // Validated here, server-side: a wipe needs the typed confirmation.
        existing_data: resolve_existing_data(form.existing_data, &form.wipe_confirmation)
            .map_err(AppError::Validation)?,
    };

    let hardware = state.hardware.lock().await.clone();
    let execution_mode = state.config.execution_mode;
    let targets = target_devices(&hardware);

    if config.existing_data == ExistingDataPolicy::WipeAll {
        // The confirmation is bound to the drives the user saw: rescan and
        // refuse if they differ (or cannot be read) before anything starts.
        let (current, scan_error) = load_existing_drives(&targets).await;
        let scan = scan_error.is_none().then_some(current.as_slice());
        if !wipe_fingerprint_confirmed(&form.wipe_fingerprint, scan) {
            tracing::warn!(
                "WIPE ALL refused for {}: the drives changed since the page was loaded; nothing was changed",
                form.hostname
            );
            let template = IndexTemplate {
                hardware,
                version: crate::update::full_version(),
                build_time: crate::update::build_timestamp(),
                layout_error: None,
                wipe_fingerprint: page_fingerprint(&current, &scan_error),
                existing_drives: current,
                existing_drives_error: scan_error,
                wipe_word: WIPE_CONFIRMATION,
                notice: Some(DRIVES_CHANGED_NOTICE.to_string()),
            };
            let html = template
                .render()
                .map_err(|e| AppError::Template(e.to_string()))?;
            return Ok(Html(html));
        }
        tracing::warn!(
            "User confirmed WIPE ALL for {}: all data on the target drive(s) will be destroyed",
            form.hostname
        );
    }

    // Eagerly build the plan to catch layout-incompatibility before spawning the
    // background task. This lets us explain the problem to the user without
    // waiting for the SSE log stream.
    if let Err(plan_err) =
        provisioning::build_provisioning_plan(config.clone(), hardware.clone(), execution_mode)
            .await
    {
        if let crate::error::BeaconError::Layout(layout_err) = &plan_err {
            tracing::warn!("Stage 2 refused the disk layout during planning; nothing was changed");
            let (existing_drives, existing_drives_error) = load_existing_drives(&targets).await;
            let template = IndexTemplate {
                wipe_fingerprint: page_fingerprint(&existing_drives, &existing_drives_error),
                notice: None,
                hardware,
                version: crate::update::full_version(),
                build_time: crate::update::build_timestamp(),
                layout_error: Some(LayoutErrorContext {
                    message: layout_err.to_string(),
                    can_wipe: wipe_can_resolve(layout_err),
                    unit_type: form.unit_type.clone(),
                    hostname: form.hostname.clone(),
                    ssh_key: form.ssh_key.clone(),
                    wifi_ssid: form.wifi_ssid.trim().to_string(),
                    wifi_country: form.wifi_country.trim().to_string(),
                }),
                existing_drives,
                existing_drives_error,
                wipe_word: WIPE_CONFIRMATION,
            };
            let html = template
                .render()
                .map_err(|e| AppError::Template(e.to_string()))?;
            return Ok(Html(html));
        }
        // All other plan errors surface as normal AppErrors.
        return Err(AppError::Beacon(plan_err));
    }

    // Plan succeeded — spawn provisioning in background, waiting for start signal from JavaScript
    spawn_with_start_signal(&state.provision_start, move || async move {
        match provisioning::provision_system(config, hardware, execution_mode).await {
            Ok(provisioned) => {
                info!("Provisioning completed successfully {provisioned:?}");
            }
            Err(e) => {
                tracing::error!("Provisioning failed: {}", e);
            }
        }
    })
    .await;

    let is_dry_run = execution_mode == crate::actions::ExecutionMode::DryRun;
    let html = ProvisionPageTemplate { is_dry_run }
        .render()
        .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}

/// Handler for provision start signal from JavaScript
pub async fn provision_start(State(state): State<AppState>) -> Result<StatusCode, AppError> {
    // Take the sender out of the mutex and signal start
    if let Some(tx) = state.provision_start.lock().await.take() {
        let _ = tx.send(());
        info!("Provisioning start signal received");
        Ok(StatusCode::OK)
    } else {
        tracing::warn!("Provision start called but no provisioning task waiting");
        Err(AppError::Validation(
            "No provisioning task waiting".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case(ExistingDataPolicy::Preserve, "", ExistingDataPolicy::Preserve)]
    #[case(ExistingDataPolicy::ReinstallOs, "", ExistingDataPolicy::ReinstallOs)]
    // A confirmation word without choosing wipe changes nothing.
    #[case(ExistingDataPolicy::Preserve, "WIPE", ExistingDataPolicy::Preserve)]
    #[case(
        ExistingDataPolicy::ReinstallOs,
        "WIPE",
        ExistingDataPolicy::ReinstallOs
    )]
    #[case(ExistingDataPolicy::WipeAll, "WIPE", ExistingDataPolicy::WipeAll)]
    #[case(ExistingDataPolicy::WipeAll, " WIPE ", ExistingDataPolicy::WipeAll)]
    fn resolves_policy(
        #[case] chosen: ExistingDataPolicy,
        #[case] confirmation: &str,
        #[case] expected: ExistingDataPolicy,
    ) {
        assert_eq!(resolve_existing_data(chosen, confirmation), Ok(expected));
    }

    /// Parse a urlencoded POST body the way the `/provision` handler does.
    async fn parse_form(extra: &str) -> Option<ProvisionForm> {
        use axum::extract::FromRequest;
        let body = format!("unit_type=mdma-909&hostname=h&ssh_key=k&{extra}");
        let request = axum::http::Request::builder()
            .method("POST")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body))
            .unwrap();
        Form::<ProvisionForm>::from_request(request, &())
            .await
            .ok()
            .map(|Form(f)| f)
    }

    #[rstest]
    #[case("", ExistingDataPolicy::Preserve)]
    #[case("existing_data=preserve", ExistingDataPolicy::Preserve)]
    #[case("existing_data=reinstall_os", ExistingDataPolicy::ReinstallOs)]
    #[case("existing_data=wipe_all", ExistingDataPolicy::WipeAll)]
    // The old checkbox fields are gone and select nothing.
    #[case("wipe_all=true&reinstall_os=true", ExistingDataPolicy::Preserve)]
    #[tokio::test]
    async fn form_parses_existing_data_choice(
        #[case] extra: &str,
        #[case] expected: ExistingDataPolicy,
    ) {
        let form = parse_form(extra).await.expect("form should parse");
        assert_eq!(form.existing_data, expected);
    }

    #[tokio::test]
    async fn form_rejects_unknown_existing_data_choice() {
        assert!(parse_form("existing_data=both").await.is_none());
    }

    #[rstest]
    #[case("")]
    #[case("wipe")]
    #[case("WIPE IT")]
    #[case("yes")]
    #[case("mdma-909")]
    fn wipe_without_correct_confirmation_is_rejected(#[case] confirmation: &str) {
        let err = resolve_existing_data(ExistingDataPolicy::WipeAll, confirmation).unwrap_err();
        assert!(err.contains("Nothing was changed"), "got: {err}");
    }

    #[test]
    fn wipe_override_is_not_offered_for_unreadable_or_foreign_data() {
        assert!(!wipe_can_resolve(&LayoutError::Unreadable {
            device: "d".into(),
            reason: "r".into()
        }));
        assert!(!wipe_can_resolve(&LayoutError::DataOnOtherDrive {
            device: "d".into(),
            other: "o".into(),
            labels: "music".into()
        }));
        assert!(wipe_can_resolve(&LayoutError::NotGpt {
            device: "d".into(),
            label: "dos".into()
        }));
        assert!(wipe_can_resolve(&LayoutError::Mismatch {
            device: "d".into(),
            expected: "e".into(),
            found: "f".into(),
            hint: String::new()
        }));
        assert!(wipe_can_resolve(&LayoutError::Unrecognised {
            device: "d".into(),
            signature: "ext4".into()
        }));
    }

    fn render(layout_error: Option<LayoutErrorContext>, drives: Vec<ExistingDrive>) -> String {
        IndexTemplate {
            hardware: HardwareInfo {
                model: "Raspberry Pi 5".into(),
                serial: None,
                memory_mb: None,
                nvme_drives: Vec::new(),
            },
            version: "v".into(),
            build_time: "t",
            layout_error,
            wipe_fingerprint: drives_fingerprint(&drives),
            existing_drives: drives,
            existing_drives_error: None,
            wipe_word: WIPE_CONFIRMATION,
            notice: None,
        }
        .render()
        .unwrap()
    }

    fn drive(device: &str, partitions: &[&str]) -> ExistingDrive {
        ExistingDrive {
            device: device.into(),
            partitions: partitions.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn fingerprint_is_deterministic_and_order_independent() {
        let a = drive("/dev/nvme0n1", &["p1 (label: boot)", "p2 (label: music)"]);
        let b = drive("/dev/nvme1n1", &["whole-disk signature: ext4"]);
        let forward = drives_fingerprint(&[a.clone(), b.clone()]);
        assert_eq!(forward, drives_fingerprint(&[a.clone(), b.clone()]));
        assert_eq!(forward, drives_fingerprint(&[b.clone(), a.clone()]));
        let shuffled = drive("/dev/nvme0n1", &["p2 (label: music)", "p1 (label: boot)"]);
        assert_eq!(forward, drives_fingerprint(&[shuffled, b]));
    }

    #[test]
    fn fingerprint_changes_when_drives_change() {
        let base = drives_fingerprint(&[drive("/dev/nvme0n1", &["p1 (label: boot)"])]);
        for other in [
            vec![drive("/dev/nvme0n1", &["p1 (label: music)"])],
            vec![drive("/dev/nvme1n1", &["p1 (label: boot)"])],
            vec![drive(
                "/dev/nvme0n1",
                &["p1 (label: boot)", "p2 (label: x)"],
            )],
            vec![],
        ] {
            assert_ne!(base, drives_fingerprint(&other));
        }
    }

    #[test]
    fn matching_fingerprint_is_confirmed() {
        let drives = vec![drive("/dev/nvme0n1", &["p1 (label: boot)"])];
        let fp = drives_fingerprint(&drives);
        assert!(wipe_fingerprint_confirmed(&fp, Some(&drives)));
    }

    #[test]
    fn mismatched_missing_or_unreadable_fingerprint_is_rejected() {
        let drives = vec![drive("/dev/nvme0n1", &["p1 (label: boot)"])];
        let fp = drives_fingerprint(&drives);
        let swapped = vec![drive("/dev/nvme0n1", &["p1 (label: music)"])];
        assert!(!wipe_fingerprint_confirmed(&fp, Some(&swapped)));
        assert!(!wipe_fingerprint_confirmed("", Some(&drives)));
        assert!(!wipe_fingerprint_confirmed("", Some(&[])));
        assert!(!wipe_fingerprint_confirmed(&fp, None));
    }

    #[test]
    fn only_target_drives_are_listed() {
        let drives = vec![
            drive("/dev/nvme0n1", &["p1"]),
            drive("/dev/nvme1n1", &["p1"]),
        ];
        let kept = limit_to_targets(drives, &["/dev/nvme1n1".to_string()]);
        assert_eq!(kept, vec![drive("/dev/nvme1n1", &["p1"])]);
    }

    #[test]
    fn wipe_forms_carry_the_fingerprint() {
        let drives = vec![drive("/dev/nvme0n1", &["p1"])];
        let fp = drives_fingerprint(&drives);
        let ctx = LayoutErrorContext {
            message: "m".into(),
            can_wipe: true,
            unit_type: "mdma-909".into(),
            hostname: "h".into(),
            ssh_key: "k".into(),
            wifi_ssid: String::new(),
            wifi_country: String::new(),
        };
        let html = render(Some(ctx), drives);
        let field = format!("name=\"wipe_fingerprint\" value=\"{fp}\"");
        assert_eq!(html.matches(&field).count(), 2, "both wipe forms");
    }

    #[test]
    fn disk_strings_are_escaped_in_the_pages() {
        let evil = "<script>alert(1)</script>";
        let evil_ssid = "<script>\"&";
        let html = render(
            Some(LayoutErrorContext {
                message: evil.into(),
                can_wipe: true,
                unit_type: "mdma-909".into(),
                hostname: evil.into(),
                ssh_key: evil.into(),
                wifi_ssid: evil_ssid.into(),
                wifi_country: evil.into(),
            }),
            vec![ExistingDrive {
                device: "/dev/nvme0n1".into(),
                partitions: vec![format!("{evil} (label: {evil})")],
            }],
        );
        assert!(!html.contains(evil), "unescaped markup in page");
        assert!(!html.contains(evil_ssid), "unescaped SSID in page");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn confirmation_page_lists_partitions_and_warns_about_music() {
        let html = render(
            None,
            vec![ExistingDrive {
                device: "/dev/nvme0n1".into(),
                partitions: vec!["/dev/nvme0n1p5 (label: music, size: 3072 MB)".into()],
            }],
        );
        assert!(html.contains("/dev/nvme0n1p5 (label: music, size: 3072 MB)"));
        assert!(html.contains("name=\"wipe_confirmation\""));
        assert!(html.contains("name=\"existing_data\""));
        assert!(html.contains("value=\"preserve\""));
        assert!(html.contains("value=\"reinstall_os\""));
        assert!(html.contains("value=\"wipe_all\""));
        assert!(!html.contains("name=\"wipe_all\""));
        assert!(!html.contains("name=\"reinstall_os\""));
        assert!(html.contains("music"));
    }

    #[test]
    fn wipe_override_hidden_when_it_cannot_help() {
        let ctx = |can_wipe| LayoutErrorContext {
            message: "m".into(),
            can_wipe,
            unit_type: "mdma-909".into(),
            hostname: "h".into(),
            ssh_key: "k".into(),
            wifi_ssid: String::new(),
            wifi_country: String::new(),
        };
        assert!(render(Some(ctx(true)), Vec::new()).contains("id=\"wipe-override\""));
        assert!(!render(Some(ctx(false)), Vec::new()).contains("id=\"wipe-override\""));
    }

    #[test]
    fn override_form_posts_wipe_all_once_and_main_form_defaults_to_preserve() {
        let html = render(
            Some(LayoutErrorContext {
                message: "m".into(),
                can_wipe: true,
                unit_type: "mdma-909".into(),
                hostname: "h".into(),
                ssh_key: "k".into(),
                wifi_ssid: String::new(),
                wifi_country: String::new(),
            }),
            Vec::new(),
        );
        let hidden = "type=\"hidden\" name=\"existing_data\" value=\"wipe_all\"";
        assert_eq!(html.matches(hidden).count(), 1);
        let preserve = html
            .find("value=\"preserve\"")
            .expect("preserve radio present");
        let tag_end = html[preserve..].find('>').expect("tag end") + preserve;
        assert!(html[preserve..tag_end].contains("checked"));
        assert!(html[preserve..tag_end].contains("autocomplete=\"off\""));
    }

    // ---- wifi ----

    const GOOD_PSK: &str = "correct horse battery";

    #[test]
    fn all_blank_wifi_is_none() {
        assert_eq!(resolve_network("", "", ""), Ok(None));
        assert_eq!(resolve_network("  ", "", " "), Ok(None));
    }

    #[rstest]
    #[case("1234567")]
    #[case("pässword-long-enough")]
    fn invalid_passphrase_error_does_not_echo_the_passphrase(#[case] psk: &str) {
        let err = resolve_network("net", psk, "SE").unwrap_err();
        assert!(err.contains("password"), "got: {err}");
        assert!(!err.contains(psk), "error leaks the passphrase: {err}");
    }

    #[test]
    fn valid_trio_resolves_and_normalises() {
        let net = resolve_network("  Home Net ", GOOD_PSK, " se ")
            .unwrap()
            .expect("some network");
        assert_eq!(net.ssid().as_str(), "Home Net");
        assert_eq!(net.country().as_str(), "SE");
    }

    #[test]
    fn passphrase_is_not_trimmed() {
        let spaced = resolve_network("net", "  spaced out  ", "SE")
            .unwrap()
            .unwrap();
        let trimmed = resolve_network("net", "spaced out", "SE").unwrap().unwrap();
        assert_ne!(spaced.psk(), trimmed.psk());
    }

    #[rstest]
    #[case("net", "", "SE")]
    #[case("net", "", "")]
    #[case("", GOOD_PSK, "SE")]
    #[case("", GOOD_PSK, "")]
    #[case("net", GOOD_PSK, "")]
    #[case("", "", "SE")]
    #[case("net", GOOD_PSK, "  ")]
    fn incomplete_wifi_is_rejected(#[case] ssid: &str, #[case] psk: &str, #[case] country: &str) {
        let err = resolve_network(ssid, psk, country).unwrap_err();
        assert!(err.ends_with("Nothing was changed."), "got: {err}");
    }

    #[rstest]
    #[case(&"s".repeat(33), GOOD_PSK, "SE", "network name")]
    #[case("net", "short", "SE", "password")]
    #[case("net", GOOD_PSK, "SWE", "country")]
    #[case("net", GOOD_PSK, "S1", "country")]
    fn invalid_wifi_values_are_rejected(
        #[case] ssid: &str,
        #[case] psk: &str,
        #[case] country: &str,
        #[case] what: &str,
    ) {
        let err = resolve_network(ssid, psk, country).unwrap_err();
        assert!(err.contains(what), "got: {err}");
        assert!(err.ends_with("Nothing was changed."), "got: {err}");
    }

    fn form_with_psk(psk: String) -> ProvisionForm {
        ProvisionForm {
            unit_type: "mdma-909".into(),
            hostname: "h".into(),
            ssh_key: "k".into(),
            existing_data: ExistingDataPolicy::Preserve,
            wipe_confirmation: String::new(),
            wipe_fingerprint: String::new(),
            wifi_ssid: "net".into(),
            wifi_psk: psk,
            wifi_country: "SE".into(),
        }
    }

    #[test]
    fn form_debug_marks_empty_and_redacted_psk() {
        assert!(format!("{:?}", form_with_psk(String::new())).contains("wifi_psk: <empty>"));
        assert!(format!("{:?}", form_with_psk(GOOD_PSK.into())).contains("wifi_psk: <redacted>"));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn form_debug_never_contains_psk(psk in "[ -~]{8,63}[g-zG-Z]") {
                let debug = format!("{:?}", form_with_psk(psk.clone()));
                prop_assert!(!debug.contains(&psk));
            }
        }
    }

    #[test]
    fn override_page_never_echoes_the_psk() {
        let psk = "Zq9-never-echo-me";
        let form = form_with_psk(psk.into());
        let html = render(
            Some(LayoutErrorContext {
                message: "m".into(),
                can_wipe: true,
                unit_type: form.unit_type.clone(),
                hostname: form.hostname.clone(),
                ssh_key: form.ssh_key.clone(),
                wifi_ssid: form.wifi_ssid.clone(),
                wifi_country: form.wifi_country.clone(),
            }),
            Vec::new(),
        );
        assert!(!html.contains(psk));
        assert!(html.contains("name=\"wifi_ssid\" value=\"net\""));
        assert!(html.contains("name=\"wifi_country\" value=\"SE\""));
        // The visible password input for the override has no value and is
        // required because an SSID was given.
        let start = html
            .find("name=\"wifi_psk\" id=\"override_wifi_psk\"")
            .unwrap();
        let end = start + html[start..].find('>').unwrap();
        let tag = &html[start..end];
        assert!(!tag.contains("value="), "got: {tag}");
        assert!(tag.contains("required"), "got: {tag}");
        assert!(tag.contains("autocomplete=\"new-password\""), "got: {tag}");
    }

    #[test]
    fn override_psk_not_required_without_ssid() {
        let html = render(
            Some(LayoutErrorContext {
                message: "m".into(),
                can_wipe: true,
                unit_type: "mdma-909".into(),
                hostname: "h".into(),
                ssh_key: "k".into(),
                wifi_ssid: String::new(),
                wifi_country: String::new(),
            }),
            Vec::new(),
        );
        let start = html
            .find("name=\"wifi_psk\" id=\"override_wifi_psk\"")
            .unwrap();
        let end = start + html[start..].find('>').unwrap();
        assert!(!html[start..end].contains("required"));
    }
}
