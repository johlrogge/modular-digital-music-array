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

/// Turn the submitted checkboxes into a policy. `WipeAll` is returned only when
/// the wipe box is ticked AND the confirmation word was typed exactly; a
/// ticked box with a missing or wrong confirmation is an error, never a
/// downgrade, so the user is not surprised by a different outcome.
fn resolve_existing_data(
    wipe_all: bool,
    reinstall_os: bool,
    confirmation: &str,
) -> Result<ExistingDataPolicy, String> {
    if wipe_all {
        if confirmation.trim() != WIPE_CONFIRMATION {
            return Err(format!(
                "Wiping the drive requires typing {} to confirm. Nothing was changed.",
                WIPE_CONFIRMATION
            ));
        }
        Ok(ExistingDataPolicy::WipeAll)
    } else if reinstall_os {
        Ok(ExistingDataPolicy::ReinstallOs)
    } else {
        Ok(ExistingDataPolicy::Preserve)
    }
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
#[derive(Debug, Deserialize)]
pub(crate) struct ProvisionForm {
    unit_type: String,
    hostname: String,
    ssh_key: String,
    /// Checkbox: reformat boot, root and var. Music and metadata are never
    /// touched, whatever this says.
    #[serde(default)]
    reinstall_os: bool,
    /// Checkbox: wipe the drive(s), destroying ALL data including music and
    /// metadata. Honoured only together with `wipe_confirmation`.
    #[serde(default)]
    wipe_all: bool,
    /// Must equal [`WIPE_CONFIRMATION`] for `wipe_all` to take effect.
    #[serde(default)]
    wipe_confirmation: String,
    /// Fingerprint of the drive list the user saw; a wipe is refused unless it
    /// matches the drives as they are when the form is posted.
    #[serde(default)]
    wipe_fingerprint: String,
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

    let config = ProvisionConfig {
        ssh_key,
        unit_type,
        hostname,
        // Validated here, server-side: a wipe needs the typed confirmation.
        existing_data: resolve_existing_data(
            form.wipe_all,
            form.reinstall_os,
            &form.wipe_confirmation,
        )
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
    #[case(false, false, "", ExistingDataPolicy::Preserve)]
    #[case(false, true, "", ExistingDataPolicy::ReinstallOs)]
    // A confirmation word without the box ticked changes nothing.
    #[case(false, false, "WIPE", ExistingDataPolicy::Preserve)]
    #[case(true, false, "WIPE", ExistingDataPolicy::WipeAll)]
    #[case(true, true, " WIPE ", ExistingDataPolicy::WipeAll)]
    fn resolves_policy(
        #[case] wipe_all: bool,
        #[case] reinstall_os: bool,
        #[case] confirmation: &str,
        #[case] expected: ExistingDataPolicy,
    ) {
        assert_eq!(
            resolve_existing_data(wipe_all, reinstall_os, confirmation),
            Ok(expected)
        );
    }

    #[rstest]
    #[case("")]
    #[case("wipe")]
    #[case("WIPE IT")]
    #[case("yes")]
    #[case("mdma-909")]
    fn wipe_without_correct_confirmation_is_rejected(#[case] confirmation: &str) {
        let err = resolve_existing_data(true, false, confirmation).unwrap_err();
        assert!(err.contains("Nothing was changed"), "got: {err}");
        // Also when reinstall is ticked: never silently downgraded.
        assert!(resolve_existing_data(true, true, confirmation).is_err());
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
        };
        let html = render(Some(ctx), drives);
        let field = format!("name=\"wipe_fingerprint\" value=\"{fp}\"");
        assert_eq!(html.matches(&field).count(), 2, "both wipe forms");
    }

    #[test]
    fn disk_strings_are_escaped_in_the_pages() {
        let evil = "<script>alert(1)</script>";
        let html = render(
            Some(LayoutErrorContext {
                message: evil.into(),
                can_wipe: true,
                unit_type: "mdma-909".into(),
                hostname: evil.into(),
                ssh_key: evil.into(),
            }),
            vec![ExistingDrive {
                device: "/dev/nvme0n1".into(),
                partitions: vec![format!("{evil} (label: {evil})")],
            }],
        );
        assert!(!html.contains(evil), "unescaped markup in page");
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
        assert!(html.contains("name=\"wipe_all\""));
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
        };
        assert!(render(Some(ctx(true)), Vec::new()).contains("id=\"wipe-override\""));
        assert!(!render(Some(ctx(false)), Vec::new()).contains("id=\"wipe-override\""));
    }
}
