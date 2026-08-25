use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use jni::{
    errors::{Result as JniResult},
    objects::{JObject, JString, JValue},
    JNIEnv,
};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Url};

const OPEN_DOCUMENT_EVENT: &str = "nomo://open-document";
const TARGET_WINDOW_LABEL: &str = "main";
const PENDING_EXTERNAL_OPEN_KEY: &str = "pendingExternalOpen:main";
const INCOMING_DIR_NAME: &str = "incoming";
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const STALE_IMPORT_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

static PENDING_URLS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
struct ExternalOpenPayload {
    #[serde(rename = "windowLabel")]
    window_label: String,
    paths: Vec<String>,
}

/// 挂在 `App::run` 回调上：消费框架转出的 `RunEvent::Opened`（微信等外部应用
/// 通过 ACTION_VIEW / ACTION_SEND 打开文件时由 tao/wry 自动触发），并把
/// content:// 等 URI 导入为本地文件后转发给前端。
pub(crate) fn handle_run_event(app: &AppHandle, event: &RunEvent) {
    match event {
        RunEvent::Opened { urls } => {
            let url_strings: Vec<String> =
                urls.iter().map(|url| url.as_str().to_string()).collect();
            crate::app_logger::info(
                "ExternalOpen",
                &format!("收到 Android 打开事件：urls={url_strings:?}"),
            );
            if app.try_state::<crate::config::ConfigManager>().is_none() {
                queue_pending(url_strings);
                crate::app_logger::info("ExternalOpen", "setup 尚未完成，暂存待处理 URL");
            } else {
                dispatch_urls(app, url_strings);
            }
        }
        _ => flush_pending(app),
    }
}

fn queue_pending(urls: Vec<String>) {
    if let Ok(mut pending) = PENDING_URLS.get_or_init(|| Mutex::new(Vec::new())).lock() {
        for url in urls {
            if !pending.contains(&url) {
                pending.push(url);
            }
        }
    }
}

fn take_pending() -> Vec<String> {
    PENDING_URLS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default()
}

fn flush_pending(app: &AppHandle) {
    let has_pending = PENDING_URLS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map(|pending| !pending.is_empty())
        .unwrap_or(false);
    if !has_pending || app.try_state::<crate::config::ConfigManager>().is_none() {
        return;
    }
    dispatch_urls(app, take_pending());
}

fn dispatch_urls(app: &AppHandle, urls: Vec<String>) {
    let mut local_paths: Vec<String> = Vec::new();
    let mut deferred: Vec<Url> = Vec::new();

    for url_string in &urls {
        match Url::parse(url_string) {
            Ok(url) => match url.scheme() {
                "file" => match url.to_file_path() {
                    Ok(path) => {
                        if has_supported_path_extension(&path) {
                            local_paths.push(path.to_string_lossy().into_owned());
                        } else {
                            crate::app_logger::info(
                                "ExternalOpen",
                                &format!("忽略不支持的文件类型：{}", path.display()),
                            );
                        }
                    }
                    Err(_) => {
                        crate::app_logger::warn(
                            "ExternalOpen",
                            "file:// URL 无法转换为本地路径",
                        );
                    }
                },
                "content" | "data" => deferred.push(url),
                scheme => {
                    crate::app_logger::info(
                        "ExternalOpen",
                        &format!("忽略不支持的 URL 协议：{scheme}"),
                    );
                }
            },
            Err(_) => {
                crate::app_logger::warn("ExternalOpen", &format!("无法解析的 URL：{url_string}"));
            }
        }
    }

    route_paths(app, local_paths);

    if deferred.is_empty() {
        return;
    }

    // content:// 与 data: 需要经 JNI 访问 ContentResolver 和 cacheDir，
    // 而 JNI 必须在 webview 线程执行；webview 未就绪或投递失败时先回队等待重试。
    let Some(window) = app.get_webview_window(TARGET_WINDOW_LABEL) else {
        queue_pending(deferred.iter().map(|url| url.to_string()).collect());
        return;
    };
    let deferred_strings: Vec<String> =
        deferred.iter().map(|url| url.to_string()).collect();
    let app_handle = app.clone();
    let import_result = window.as_ref().with_webview(move |platform_webview| {
        platform_webview.jni_handle().exec(move |env, activity, _| {
            let mut imported: Vec<String> = Vec::new();
            for url in deferred {
                match with_exception_guard(env, |env| import_url(env, activity, &url)) {
                    Ok(Some(path)) => imported.push(path),
                    Ok(None) => {
                        crate::app_logger::info(
                            "ExternalOpen",
                            &format!("跳过无法导入的内容：{url}"),
                        );
                    }
                    Err(error) => {
                        crate::app_logger::error(
                            "ExternalOpen",
                            &format!("导入 {url} 失败：{error}"),
                        );
                    }
                }
            }
            if !imported.is_empty() {
                route_paths(&app_handle, imported);
            }
        })
    });
    if let Err(error) = import_result {
        crate::app_logger::error(
            "ExternalOpen",
            &format!("JNI 任务投递失败，回退等待重试：{error}"),
        );
        queue_pending(deferred_strings);
    }
}

fn import_url(
    env: &mut JNIEnv,
    activity: &JObject,
    url: &Url,
) -> Result<Option<String>, String> {
    let incoming_dir = incoming_dir(env, activity)?;
    clean_stale_imports(&incoming_dir)?;

    match url.scheme() {
        "content" => {
            let resolver = env
                .call_method(
                    activity,
                    "getContentResolver",
                    "()Landroid/content/ContentResolver;",
                    &[],
                )
                .and_then(|value| value.l())
                .map_err(|error| format!("获取 ContentResolver 失败：{error}"))?;
            import_content_uri(env, resolver, url.as_str(), &incoming_dir)
        }
        "data" => import_data_url(url, &incoming_dir).map(Some),
        _ => Ok(None),
    }
}

fn import_content_uri(
    env: &mut JNIEnv,
    resolver: JObject,
    uri_string: &str,
    incoming_dir: &Path,
) -> Result<Option<String>, String> {
    let null_object = JObject::null();
    let uri_jstring = env
        .new_string(uri_string)
        .map_err(|error| format!("创建 URI 字符串失败：{error}"))?;
    let uri = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&uri_jstring)],
        )
        .and_then(|value| value.l())
        .map_err(|error| format!("解析 URI 失败：{error}"))?;
    if uri.is_null() {
        return Ok(None);
    }

    let display_name = query_display_name(env, &resolver, &uri, &null_object)
        .or_else(|| last_path_segment(env, &uri));
    let Some(display_name) = display_name else {
        crate::app_logger::info("ExternalOpen", &format!("URI 缺少文件名：{uri_string}"));
        return Ok(None);
    };
    if !has_supported_name_extension(&display_name) {
        crate::app_logger::info(
            "ExternalOpen",
            &format!("忽略不支持的文件类型：{display_name}"),
        );
        return Ok(None);
    }

    let target_path = unique_target_path(incoming_dir, &display_name);
    let input_stream = env
        .call_method(
            &resolver,
            "openInputStream",
            "(Landroid/net/Uri;)Ljava/io/InputStream;",
            &[JValue::Object(&uri)],
        )
        .and_then(|value| value.l())
        .map_err(|error| format!("打开输入流失败（可能没有读取权限）：{error}"))?;
    if input_stream.is_null() {
        return Err("openInputStream 返回空".to_string());
    }

    let target_jstring = env
        .new_string(target_path.to_string_lossy().as_ref())
        .map_err(|error| format!("创建目标路径字符串失败：{error}"))?;
    let output_stream = env
        .new_object(
            "java/io/FileOutputStream",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&target_jstring)],
        )
        .map_err(|error| format!("创建输出流失败：{error}"))?;

    let copy_result =
        copy_stream(env, &input_stream, &output_stream).map_err(|error| format!("拷贝内容失败：{error}"));

    // 无论拷贝成败都关闭流，失败时清理半成品文件
    let _ = env.call_method(&input_stream, "close", "()V", &[]);
    let _ = env.call_method(&output_stream, "close", "()V", &[]);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_file(&target_path);
        return Err(error);
    }

    crate::app_logger::info(
        "ExternalOpen",
        &format!(
            "content URI 已导入：{uri_string} → {}",
            target_path.display()
        ),
    );
    Ok(Some(target_path.to_string_lossy().into_owned()))
}

fn import_data_url(url: &Url, incoming_dir: &Path) -> Result<String, String> {
    // data:[<mediatype>][;base64],<data>，此处只处理微信分享文本场景
    if url.path().contains(";base64,") {
        return Err("暂不支持 base64 data URL".to_string());
    }
    let (_, encoded) = url
        .path()
        .split_once(',')
        .ok_or_else(|| "data URL 缺少数据段".to_string())?;
    let decoded = percent_decode_str(encoded).decode_utf8_lossy();
    if decoded.trim().is_empty() {
        return Err("分享的文本内容为空".to_string());
    }

    let target_path = incoming_dir.join(format!("shared-{}.md", now_unix_millis()));
    std::fs::write(&target_path, decoded.as_bytes())
        .map_err(|error| format!("写入分享文本失败：{error}"))?;
    crate::app_logger::info(
        "ExternalOpen",
        &format!("分享文本已导入 → {}", target_path.display()),
    );
    Ok(target_path.to_string_lossy().into_owned())
}

fn query_display_name(
    env: &mut JNIEnv,
    resolver: &JObject,
    uri: &JObject,
    null_object: &JObject,
) -> Option<String> {
    let projection_key = env.new_string("_display_name").ok()?;
    let projection = env
        .new_object_array(1, "java/lang/String", projection_key)
        .ok()?;
    let cursor = env
        .call_method(
            resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[
                JValue::Object(uri),
                JValue::Object(&projection),
                JValue::Object(null_object),
                JValue::Object(null_object),
                JValue::Object(null_object),
            ],
        )
        .and_then(|value| value.l())
        .ok()?;
    if cursor.is_null() {
        return None;
    }
    let moved = env
        .call_method(&cursor, "moveToFirst", "()Z", &[])
        .and_then(|value| value.z())
        .unwrap_or(false);
    let name = if moved {
        env.call_method(&cursor, "getString", "(I)Ljava/lang/String;", &[JValue::Int(0)])
            .and_then(|value| value.l())
            .ok()
            .filter(|value| !value.is_null())
            .and_then(|value| jstring_to_string(env, value))
    } else {
        None
    };
    let _ = env.call_method(&cursor, "close", "()V", &[]);
    name.filter(|name| !name.trim().is_empty())
}

fn last_path_segment(env: &mut JNIEnv, uri: &JObject) -> Option<String> {
    env.call_method(uri, "getLastPathSegment", "()Ljava/lang/String;", &[])
        .and_then(|value| value.l())
        .ok()
        .filter(|value| !value.is_null())
        .and_then(|value| jstring_to_string(env, value))
        .filter(|segment| !segment.trim().is_empty())
}

fn copy_stream(env: &mut JNIEnv, input: &JObject, output: &JObject) -> JniResult<()> {
    let buffer = env.new_byte_array(COPY_BUFFER_BYTES as jni::sys::jsize)?;
    loop {
        let read = env
            .call_method(input, "read", "([B)I", &[JValue::Object(&buffer)])?
            .i()?;
        if read <= 0 {
            break;
        }
        env.call_method(
            output,
            "write",
            "([BII)V",
            &[JValue::Object(&buffer), JValue::Int(0), JValue::Int(read)],
        )?;
    }
    Ok(())
}

fn incoming_dir(env: &mut JNIEnv, activity: &JObject) -> Result<PathBuf, String> {
    let cache_file = env
        .call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])
        .and_then(|value| value.l())
        .map_err(|error| format!("获取缓存目录失败：{error}"))?;
    let cache_path = env
        .call_method(&cache_file, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .and_then(|value| value.l())
        .map_err(|error| format!("读取缓存目录路径失败：{error}"))
        .ok()
        .filter(|value| !value.is_null())
        .and_then(|value| jstring_to_string(env, value))
        .ok_or_else(|| "获取缓存目录路径返回空".to_string())?;

    let incoming_dir = PathBuf::from(cache_path).join(INCOMING_DIR_NAME);
    std::fs::create_dir_all(&incoming_dir)
        .map_err(|error| format!("创建导入目录失败：{error}"))?;
    Ok(incoming_dir)
}

// 执行 JNI 任务后清理可能挂起的 Java 异常，避免污染同一线程上的后续调用
fn with_exception_guard<T>(
    env: &mut JNIEnv,
    task: impl FnOnce(&mut JNIEnv) -> Result<T, String>,
) -> Result<T, String> {
    let result = task(env);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result
}

fn jstring_to_string(env: &mut JNIEnv, value: JObject) -> Option<String> {
    let jstring = JString::from(value);
    let text = env.get_string(&jstring).ok()?;
    Some(text.to_string_lossy().into_owned())
}

fn has_supported_path_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(is_supported_extension)
        .unwrap_or(false)
}

fn has_supported_name_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(is_supported_extension)
        .unwrap_or(false)
}

fn is_supported_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "md" | "markdown" | "txt" | "json"
    )
}

fn sanitize_file_name(raw_name: &str) -> String {
    let cleaned: String = raw_name
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        format!("imported-{}.md", now_unix_millis())
    } else {
        trimmed
    }
}

fn unique_target_path(incoming_dir: &Path, raw_name: &str) -> PathBuf {
    let safe_name = sanitize_file_name(raw_name);
    let candidate = incoming_dir.join(&safe_name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = Path::new(&safe_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("imported")
        .to_string();
    let extension = Path::new(&safe_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_else(|| "txt".to_string());

    for index in 1..1000u32 {
        let numbered = incoming_dir.join(format!("{stem}-{index}.{extension}"));
        if !numbered.exists() {
            return numbered;
        }
    }
    incoming_dir.join(format!("{stem}-{}.{extension}", now_unix_millis()))
}

fn clean_stale_imports(incoming_dir: &Path) -> Result<(), String> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let entries = std::fs::read_dir(incoming_dir)
        .map_err(|error| format!("读取导入目录失败：{error}"))?;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(now_secs);
        if now_secs.saturating_sub(modified_secs) > STALE_IMPORT_MAX_AGE_SECS {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 把已就绪的本地路径交给前端：先持久化到设置（冷启动时前端尚未开始监听事件，
/// 由启动恢复逻辑兜底消费），再广播打开事件并尝试唤起主窗口。
fn route_paths(app: &AppHandle, mut paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    paths.sort();
    paths.dedup();

    let persist_result = serde_json::to_string(&paths)
        .map_err(|error| error.to_string())
        .and_then(|value_json| {
            crate::config::commands::update_app_setting(
                app.clone(),
                crate::models::SettingInput {
                    key: PENDING_EXTERNAL_OPEN_KEY.to_string(),
                    value_json,
                },
            )
        });
    if let Err(error) = persist_result {
        crate::app_logger::error("ExternalOpen", &format!("持久化待打开路径失败：{error}"));
    }

    if let Some(window) = app.get_webview_window(TARGET_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }
    if let Err(error) = app.emit(
        OPEN_DOCUMENT_EVENT,
        ExternalOpenPayload {
            window_label: TARGET_WINDOW_LABEL.to_string(),
            paths,
        },
    ) {
        crate::app_logger::error("ExternalOpen", &format!("发送打开文档事件失败：{error}"));
    }
}
