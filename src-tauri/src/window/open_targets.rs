use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

const OPEN_DOCUMENT_EVENT: &str = "nomo://open-document";
const RESERVATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum OpenTargetInput {
    Documents { paths: Vec<String> },
    Folder { path: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowOpenTargetsInput {
    pub(crate) folder_path: Option<String>,
    #[serde(default)]
    pub(crate) file_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub(crate) enum OpenTargetRouteDecision {
    Handled,
    ActivateCurrent {
        target: OpenTargetInput,
    },
    OpenCurrent {
        target: OpenTargetInput,
    },
    CreateWindow {
        #[serde(rename = "windowLabel")]
        window_label: String,
        target: OpenTargetInput,
    },
}

#[derive(Default)]
pub(crate) struct OpenTargetRegistry {
    state: Mutex<RegistryState>,
    window_sequence: AtomicU64,
}

#[derive(Default)]
struct RegistryState {
    windows: HashMap<String, WindowTargetSnapshot>,
    reservations: HashMap<String, TargetReservation>,
}

#[derive(Default)]
struct WindowTargetSnapshot {
    folder_key: Option<String>,
    file_keys: HashSet<String>,
}

struct TargetReservation {
    target_keys: HashSet<String>,
    created_at: Instant,
}

#[derive(Clone, Serialize)]
struct ExternalOpenPayload {
    #[serde(rename = "windowLabel")]
    window_label: String,
    paths: Vec<String>,
}

#[tauri::command]
pub(crate) fn sync_window_open_targets(
    window: WebviewWindow,
    registry: State<'_, OpenTargetRegistry>,
    input: WindowOpenTargetsInput,
) -> Result<(), String> {
    let folder_key = input.folder_path.as_deref().and_then(normalize_target_path);
    let file_keys = input
        .file_paths
        .iter()
        .filter_map(|path| normalize_target_path(path))
        .collect::<HashSet<_>>();

    let mut state = registry
        .state
        .lock()
        .map_err(|_| "锁定窗口目标注册表失败".to_string())?;
    state.reservations.remove(window.label());
    state.windows.insert(
        window.label().to_string(),
        WindowTargetSnapshot {
            folder_key,
            file_keys,
        },
    );
    Ok(())
}

#[tauri::command]
pub(crate) fn prepare_open_target_window(
    app: AppHandle,
    window: WebviewWindow,
    registry: State<'_, OpenTargetRegistry>,
    target: OpenTargetInput,
    create_if_missing: bool,
) -> Result<OpenTargetRouteDecision, String> {
    let current_label = window.label().to_string();
    let (existing_documents, current_target, remaining_target) = {
        let alive_labels = app.webview_windows().into_keys().collect::<HashSet<_>>();
        let mut state = registry
            .state
            .lock()
            .map_err(|_| "锁定窗口目标注册表失败".to_string())?;
        prune_registry(&mut state, &alive_labels);
        resolve_existing_targets(&state, &current_label, target)
    };

    for (label, paths) in existing_documents {
        focus_document_window(&app, &label);
        if !paths.is_empty() {
            let Some(existing_window) = app.get_webview_window(&label) else {
                continue;
            };
            existing_window
                .emit(
                    OPEN_DOCUMENT_EVENT,
                    ExternalOpenPayload {
                        window_label: label,
                        paths,
                    },
                )
                .map_err(|error| format!("发送已有文件定位事件失败：{error}"))?;
        }
    }

    if remaining_target.is_none() {
        if let Some(target) = current_target {
            return Ok(OpenTargetRouteDecision::ActivateCurrent { target });
        }
        return Ok(OpenTargetRouteDecision::Handled);
    }
    let Some(remaining_target) = remaining_target else {
        unreachable!();
    };
    if !create_if_missing {
        return Ok(OpenTargetRouteDecision::OpenCurrent {
            target: remaining_target,
        });
    }

    let target_keys = target_keys(&remaining_target);
    if target_keys.is_empty() {
        return Ok(OpenTargetRouteDecision::Handled);
    }

    let window_label = registry.next_window_label(&app)?;
    {
        let mut state = registry
            .state
            .lock()
            .map_err(|_| "锁定窗口目标注册表失败".to_string())?;
        state.reservations.insert(
            window_label.clone(),
            TargetReservation {
                target_keys,
                created_at: Instant::now(),
            },
        );
    }

    if let Err(error) = persist_pending_target(&app, &window_label, &remaining_target) {
        registry.release_reservation(&window_label);
        return Err(error);
    }

    Ok(OpenTargetRouteDecision::CreateWindow {
        window_label,
        target: remaining_target,
    })
}

#[tauri::command]
pub(crate) fn release_open_target_reservation(
    app: AppHandle,
    registry: State<'_, OpenTargetRegistry>,
    window_label: String,
) -> Result<(), String> {
    registry.release_reservation(&window_label);
    clear_pending_target(&app, &window_label)
}

impl OpenTargetRegistry {
    pub(crate) fn forget_window(&self, label: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.windows.remove(label);
            state.reservations.remove(label);
        }
    }

    pub(crate) fn next_window_label(&self, app: &AppHandle) -> Result<String, String> {
        for _ in 0..32 {
            let sequence = self.window_sequence.fetch_add(1, Ordering::Relaxed) + 1;
            let label = format!("window-{}-{sequence}", crate::config::now_ts());
            let reserved = self
                .state
                .lock()
                .map_err(|_| "锁定窗口目标注册表失败".to_string())?
                .reservations
                .contains_key(&label);
            if !reserved && app.get_webview_window(&label).is_none() {
                return Ok(label);
            }
        }
        Err("无法生成唯一窗口标识".to_string())
    }

    fn release_reservation(&self, label: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.reservations.remove(label);
        }
    }
}

fn resolve_existing_targets(
    state: &RegistryState,
    current_label: &str,
    target: OpenTargetInput,
) -> (
    BTreeMap<String, Vec<String>>,
    Option<OpenTargetInput>,
    Option<OpenTargetInput>,
) {
    match target {
        OpenTargetInput::Folder { path } => {
            let Some(key) = normalize_target_path(&path) else {
                return (BTreeMap::new(), None, None);
            };
            if let Some(owner) = find_target_owner(state, current_label, &key, true) {
                if owner == current_label {
                    return (
                        BTreeMap::new(),
                        Some(OpenTargetInput::Folder { path }),
                        None,
                    );
                }
                return (BTreeMap::from([(owner, Vec::new())]), None, None);
            }
            (
                BTreeMap::new(),
                None,
                Some(OpenTargetInput::Folder { path }),
            )
        }
        OpenTargetInput::Documents { paths } => {
            let mut owners = BTreeMap::<String, Vec<String>>::new();
            let mut current_paths = Vec::new();
            let mut remaining = Vec::new();
            let mut seen = HashSet::new();
            for path in paths {
                let Some(key) = normalize_target_path(&path) else {
                    continue;
                };
                if !seen.insert(key.clone()) {
                    continue;
                }
                if let Some(owner) = find_target_owner(state, current_label, &key, false) {
                    if owner == current_label {
                        current_paths.push(path);
                    } else {
                        owners.entry(owner).or_default().push(path);
                    }
                } else {
                    remaining.push(path);
                }
            }
            let remaining_target =
                (!remaining.is_empty()).then_some(OpenTargetInput::Documents { paths: remaining });
            let current_target =
                (!current_paths.is_empty()).then_some(OpenTargetInput::Documents {
                    paths: current_paths,
                });
            (owners, current_target, remaining_target)
        }
    }
}

fn find_target_owner(
    state: &RegistryState,
    current_label: &str,
    target_key: &str,
    folder: bool,
) -> Option<String> {
    if owns_target(state.windows.get(current_label), target_key, folder) {
        return Some(current_label.to_string());
    }

    let mut labels = state.windows.keys().cloned().collect::<Vec<_>>();
    labels.sort();
    for label in labels {
        if label != current_label && owns_target(state.windows.get(&label), target_key, folder) {
            return Some(label);
        }
    }

    let mut reserved_labels = state.reservations.keys().cloned().collect::<Vec<_>>();
    reserved_labels.sort();
    reserved_labels.into_iter().find(|label| {
        state
            .reservations
            .get(label)
            .is_some_and(|reservation| reservation.target_keys.contains(target_key))
    })
}

fn owns_target(snapshot: Option<&WindowTargetSnapshot>, target_key: &str, folder: bool) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    if folder {
        snapshot.folder_key.as_deref() == Some(target_key)
    } else {
        snapshot.file_keys.contains(target_key)
    }
}

fn prune_registry(state: &mut RegistryState, alive_labels: &HashSet<String>) {
    state
        .windows
        .retain(|label, _| alive_labels.contains(label));
    state
        .reservations
        .retain(|_, reservation| reservation.created_at.elapsed() <= RESERVATION_TIMEOUT);
}

fn target_keys(target: &OpenTargetInput) -> HashSet<String> {
    match target {
        OpenTargetInput::Folder { path } => normalize_target_path(path).into_iter().collect(),
        OpenTargetInput::Documents { paths } => paths
            .iter()
            .filter_map(|path| normalize_target_path(path))
            .collect(),
    }
}

fn normalize_target_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(trimmed);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(&candidate))
            .unwrap_or(candidate)
    };
    let resolved = fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut normalized = resolved.to_string_lossy().replace('\\', "/");
    if let Some(path_without_prefix) = normalized.strip_prefix("//?/UNC/") {
        normalized = format!("//{path_without_prefix}");
    } else if let Some(path_without_prefix) = normalized.strip_prefix("//?/") {
        normalized = path_without_prefix.to_string();
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    #[cfg(target_os = "windows")]
    {
        normalized = normalized.to_lowercase();
    }
    Some(normalized)
}

fn persist_pending_target(
    app: &AppHandle,
    label: &str,
    target: &OpenTargetInput,
) -> Result<(), String> {
    match target {
        OpenTargetInput::Documents { paths } => {
            crate::window::external_open::persist_pending_external_open(app, label, paths)
        }
        OpenTargetInput::Folder { path } => {
            crate::window::external_open::persist_pending_external_folder_open(app, label, path)
        }
    }
}

fn clear_pending_target(app: &AppHandle, label: &str) -> Result<(), String> {
    let empty_value = serde_json::to_string("")
        .map_err(|error| format!("序列化待打开目标清理值失败：{error}"))?;
    for key in [
        crate::window::external_open::pending_external_open_key(label),
        crate::window::external_open::pending_external_folder_key(label),
    ] {
        crate::config::commands::update_app_setting(
            app.clone(),
            crate::models::SettingInput {
                key,
                value_json: empty_value.clone(),
            },
        )?;
    }
    Ok(())
}

fn focus_document_window(app: &AppHandle, label: &str) {
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    crate::window::os::bring_window_to_front(&window);
}
