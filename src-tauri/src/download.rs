//! Explicit, single-episode MP4 export. All network access stays in TicketStore.
use crate::{
    api::Api,
    playback::{Identity, TicketStore},
};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::Manager;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Serialize)]
pub struct Status {
    pub phase: String,
    pub name: String,
    pub percent: f64,
    pub bytes: u64,
    pub message: String,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            name: String::new(),
            percent: 0.0,
            bytes: 0,
            message: String::new(),
        }
    }
}
fn busy(phase: &str) -> bool {
    matches!(phase, "preparing" | "downloading")
}
pub struct Downloads {
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}
impl Default for Downloads {
    fn default() -> Self {
        Self {
            status: Default::default(),
            cancel: Default::default(),
        }
    }
}
fn safe_name(title: &str, ep: u32) -> String {
    let clean: String = title
        .chars()
        .take(70)
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("HGhub-{}-EP{ep:02}", clean.trim_matches(['.', ' ']))
}
fn remux_args(input: &str, output: &Path) -> Vec<String> {
    [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-n",
        "-protocol_whitelist",
        "http,tcp,crypto",
        "-allowed_extensions",
        "ALL",
        "-extension_picky",
        "0",
        "-rw_timeout",
        "45000000",
        "-format_whitelist",
        "hls,mpegts,mov,aac,mp3",
        "-f",
        "hls",
        "-i",
        input,
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-c",
        "copy",
        "-movflags",
        "+faststart",
        "-progress",
        "pipe:1",
        "-nostats",
        "-f",
        "mp4",
        output.to_str().unwrap_or(""),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
fn apply_progress(status: &mut Status, line: &str, duration: f64) {
    if let Some(value) = line
        .strip_prefix("total_size=")
        .and_then(|v| v.parse::<u64>().ok())
    {
        status.bytes = value;
    }
    if let Some(value) = line
        .strip_prefix("out_time_us=")
        .and_then(|v| v.parse::<u64>().ok())
    {
        if duration.is_finite() && duration > 0.0 {
            status.percent = (value as f64 / 1_000_000.0 / duration * 100.0).clamp(0.0, 99.0);
        }
    }
}
fn strict_manifest(text: &str, prefix: &str) -> Result<String, String> {
    let bad = || "播放清单含不支持的资源引用，已停止下载".to_string();
    let resource = |raw: &str| -> Result<(), String> {
        let url = url::Url::parse(raw).map_err(|_| bad())?;
        if url.username() != ""
            || url.password().is_some()
            || url.fragment().is_some()
            || !(url.as_str().starts_with(&format!("{prefix}/hls?"))
                || url.as_str().starts_with(&format!("{prefix}/seg?")))
        {
            return Err(bad());
        }
        let pairs: Vec<_> = url.query_pairs().collect();
        if pairs.len() != 2
            || !pairs.iter().any(|(k, v)| k == "p" && !v.is_empty())
            || !pairs.iter().any(|(k, v)| k == "r" && !v.is_empty())
        {
            return Err(bad());
        }
        Ok(())
    };
    let mut output = String::new();
    for line in text.lines().map(str::trim) {
        if line.contains("{$") || line.chars().any(|c| c == '\\' || c.is_control()) {
            return Err(bad());
        }
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            resource(line)?;
            output.push_str(line);
            output.push('\n');
            continue;
        }
        let tag = line.split(':').next().unwrap_or("");
        let attrs = matches!(
            tag,
            "#EXT-X-KEY"
                | "#EXT-X-MAP"
                | "#EXT-X-MEDIA"
                | "#EXT-X-SESSION-KEY"
                | "#EXT-X-I-FRAME-STREAM-INF"
                | "#EXT-X-STREAM-INF"
                | "#EXT-X-START"
                | "#EXT-X-SERVER-CONTROL"
                | "#EXT-X-PART-INF"
                | "#EXT-X-PART"
                | "#EXT-X-PRELOAD-HINT"
                | "#EXT-X-RENDITION-REPORT"
                | "#EXT-X-SKIP"
                | "#EXT-X-DATERANGE"
        );
        if attrs {
            let (_, values) = crate::media::hls_attributes(line).ok_or_else(bad)?;
            if line.matches('"').count() % 2 != 0 {
                return Err(bad());
            }
            let mut canonical = Vec::new();
            let mut has_uri = false;
            for attr in values {
                let (key, value) = attr.split_once('=').ok_or_else(bad)?;
                let key = key.trim();
                let value = value.trim();
                if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(bad());
                }
                if key.eq_ignore_ascii_case("URI") {
                    if has_uri {
                        return Err(bad());
                    }
                    has_uri = true;
                    let uri = value
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .filter(|s| !s.contains('"'))
                        .ok_or_else(bad)?;
                    resource(uri)?;
                    canonical.push(format!("URI=\"{uri}\""));
                } else {
                    if value.starts_with('"') {
                        if !value.ends_with('"')
                            || value.len() < 2
                            || value[1..value.len() - 1].contains('"')
                        {
                            return Err(bad());
                        }
                    } else if value.is_empty()
                        || value
                            .chars()
                            .any(|c| c.is_whitespace() || matches!(c, '=' | '"' | '\''))
                    {
                        return Err(bad());
                    }
                    canonical.push(format!("{key}={value}"));
                }
            }
            output.push_str(&format!("{tag}:{}\n", canonical.join(",")));
        } else if matches!(
            tag,
            "#EXTM3U"
                | "#EXT-X-VERSION"
                | "#EXT-X-TARGETDURATION"
                | "#EXT-X-MEDIA-SEQUENCE"
                | "#EXT-X-DISCONTINUITY-SEQUENCE"
                | "#EXT-X-ENDLIST"
                | "#EXT-X-PLAYLIST-TYPE"
                | "#EXT-X-I-FRAMES-ONLY"
                | "#EXT-X-INDEPENDENT-SEGMENTS"
                | "#EXTINF"
                | "#EXT-X-BYTERANGE"
                | "#EXT-X-DISCONTINUITY"
                | "#EXT-X-PROGRAM-DATE-TIME"
                | "#EXT-X-GAP"
                | "#EXT-X-BITRATE"
        ) {
            output.push_str(line);
            output.push('\n');
        }
    }
    Ok(output)
}
fn publish(partial: &Path, output: &Path, result: Result<(), String>) -> Result<(), String> {
    let result = result.and_then(|_| {
        if std::fs::metadata(partial)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            std::fs::hard_link(partial, output)
                .map_err(|_| "保存 MP4 失败，请检查剩余空间和文件系统".to_string())
        } else {
            Err("导出结果为空，请重试".into())
        }
    });
    let _ = std::fs::remove_file(partial);
    result
}
fn ffmpeg_path() -> Result<PathBuf, String> {
    let binary = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(path) = std::env::var_os("HGHUB_FFMPEG")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
    {
        return Ok(path);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        for path in [dir.join("ffmpeg").join(binary), dir.join(binary)] {
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths).filter(|p| p.is_absolute()) {
            let path = dir.join(binary);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err("MP4 导出组件尚未安装，请运行客户端目录中的“安装下载组件.ps1”后重试".into())
}
fn directory(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .download_dir()
        .map(|p| p.join("HGhub"))
        .map_err(|_| "无法找到系统下载目录".into())
}

#[tauri::command]
pub fn download_status(state: tauri::State<'_, Downloads>) -> Status {
    state.status.lock().unwrap().clone()
}
#[tauri::command]
pub fn download_cancel(state: tauri::State<'_, Downloads>) {
    state.cancel.store(true, Ordering::Relaxed);
}
#[tauri::command]
pub fn download_open_folder(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = directory(&app)?;
    std::fs::create_dir_all(&path).map_err(|_| "无法创建下载目录")?;
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<&str>)
        .map_err(|_| "无法打开下载目录".into())
}
#[tauri::command]
pub async fn download_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, Downloads>,
    id: i64,
    ep: u32,
    title: String,
    duration: f64,
) -> Result<Status, String> {
    if id <= 0 || ep == 0 || ep > 10000 {
        return Err("剧集编号无效".into());
    }
    if !cfg!(target_os = "windows") {
        return Err("此版本仅支持 Windows 客户端下载".into());
    }
    let ffmpeg = ffmpeg_path()?;
    let dir = directory(&app)?;
    std::fs::create_dir_all(&dir).map_err(|_| "无法创建下载目录，请检查磁盘权限")?;
    let name = format!(
        "{}-{}.mp4",
        safe_name(&title, ep),
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let output = dir.join(&name);
    let partial = output.with_extension("part.mp4");
    let initial = Status {
        phase: "preparing".into(),
        name,
        ..Default::default()
    };
    {
        let mut status = state.status.lock().unwrap();
        if busy(&status.phase) {
            return Err("已有下载正在进行，请完成或取消后再下载".into());
        }
        *status = initial.clone();
        state.cancel.store(false, Ordering::Relaxed);
    }
    let status = state.status.clone();
    let cancel = state.cancel.clone();
    let starting_identity = Identity::current(&app.state::<Api>());
    tauri::async_runtime::spawn(async move {
        let result = export_episode(
            &app,
            id,
            ep,
            &ffmpeg,
            &partial,
            status.clone(),
            cancel.clone(),
            duration,
        )
        .await;
        let result = if cancel.load(Ordering::Relaxed)
            || Identity::current(&app.state::<Api>()) != starting_identity
        {
            cancel.store(true, Ordering::Relaxed);
            Err("下载已取消".into())
        } else {
            result
        };
        let result = publish(&partial, &output, result);
        let mut current = status.lock().unwrap();
        match result {
            Ok(()) => {
                current.phase = "completed".into();
                current.percent = 100.0;
                current.message = "已保存到 下载/HGhub".into();
            }
            Err(error) => {
                current.phase = if cancel.load(Ordering::Relaxed) {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                current.message = error;
            }
        }
    });
    Ok(initial)
}

#[derive(Deserialize)]
struct ResourceQuery {
    p: String,
    r: String,
}
#[derive(Clone)]
struct Bridge {
    app: tauri::AppHandle,
    tickets: Arc<TicketStore>,
    prefix: String,
}
async fn media(
    State(state): State<Bridge>,
    Query(query): Query<ResourceQuery>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let range = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            v.len() <= 100
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || b"bytes=,- ".contains(&b))
        });
    let api = state.app.state::<Api>();
    match state.tickets.serve(&api, &query.p, &query.r, range).await {
        Ok(response)
            if response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("mpegurl")) =>
        {
            let (mut parts, body) = response.into_parts();
            let body = match axum::body::to_bytes(body, 1024 * 1024).await {
                Ok(body) => body,
                Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            };
            match std::str::from_utf8(&body)
                .ok()
                .and_then(|text| strict_manifest(text, &state.prefix).ok())
            {
                Some(text) => {
                    parts.headers.remove("content-length");
                    Response::from_parts(parts, axum::body::Body::from(text))
                }
                None => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
        Ok(response) if !uri.path().ends_with("/hls") => response,
        Ok(_) => StatusCode::BAD_GATEWAY.into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "media unavailable").into_response(),
    }
}
async fn export_episode(
    app: &tauri::AppHandle,
    id: i64,
    ep: u32,
    ffmpeg: &Path,
    partial: &Path,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    duration: f64,
) -> Result<(), String> {
    let tickets = Arc::new(TicketStore::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "无法启动本地下载代理")?;
    let address = listener.local_addr().map_err(|_| "无法读取下载代理地址")?;
    let capability = uuid::Uuid::new_v4().simple().to_string();
    let prefix = format!("http://{address}/{capability}");
    let api = app.state::<Api>();
    let identity = Identity::current(&api);
    let play = tickets.prepare(&api, id, ep, &prefix).await?;
    if cancel.load(Ordering::Relaxed) {
        return Err("下载已取消".into());
    }
    let input = play["video_url"].as_str().ok_or("没有可下载的播放地址")?;
    let router = Router::new()
        .route(&format!("/{capability}/hls"), get(media))
        .route(&format!("/{capability}/seg"), get(media))
        .with_state(Bridge {
            app: app.clone(),
            tickets,
            prefix: prefix.clone(),
        });
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let app = app.clone();
    let result = run_remux(
        ffmpeg,
        input,
        partial,
        status,
        cancel,
        duration,
        move || Identity::current(&app.state::<Api>()) == identity,
    )
    .await;
    server.abort();
    result
}
async fn run_remux(
    ffmpeg: &Path,
    input: &str,
    partial: &Path,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    duration: f64,
    identity_valid: impl Fn() -> bool,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(ffmpeg);
    command
        .args(remux_args(input, partial))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(test)]
    command.stderr(Stdio::inherit()); // Only synthetic fixtures invoke this during tests.
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    async {
        let mut child = command.spawn().map_err(|_| "无法启动 MP4 导出组件")?;
        status.lock().unwrap().phase="downloading".into();
        let mut lines = BufReader::new(child.stdout.take().ok_or("无法读取下载进度")?).lines();
        let mut check = tokio::time::interval(Duration::from_millis(250));
        let deadline = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(deadline);
        let mut stdout_open = true;
        loop {
            tokio::select! {
                exit = child.wait() => return if exit.map_err(|_| "下载进程异常")?.success() {
                    if cancel.load(Ordering::Relaxed)||!identity_valid() {Err("下载已取消".into())}else{Ok(())}
                } else { Err("下载失败：媒体已过期、连接中断或格式不兼容，请重试".into()) },
                line = lines.next_line(), if stdout_open => {
                    match line {
                        Ok(Some(line)) => apply_progress(&mut status.lock().unwrap(), &line, duration),
                        _ => stdout_open = false,
                    }
                }
                _ = check.tick() => {
                    if cancel.load(Ordering::Relaxed) || !identity_valid() {
                        cancel.store(true, Ordering::Relaxed);
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err("下载已取消（主动取消、账号或线路发生变化）".into());
                    }
                }
                _ = &mut deadline => {
                    let _ = child.kill().await; let _ = child.wait().await;
                    return Err("下载超时，请重试".into());
                }
            }
        }
    }.await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_never_escapes_directory_or_uses_windows_reserved_names() {
        let name = safe_name("../CON:<bad>\\title", 3);
        assert!(!name.contains(['/', '\\', ':', '<', '>']));
        assert!(name.starts_with("HGhub-"));
        assert!(name.ends_with("-EP03"));
        assert!(safe_name(&"长".repeat(200), 1).chars().count() < 100);
    }
    #[test]
    fn ffmpeg_input_is_local_and_protocols_cannot_read_local_files() {
        let args = remux_args(
            "http://127.0.0.1:1234/cap/hls?p=t&r=r",
            std::path::Path::new("out.part.mp4"),
        );
        assert!(args
            .windows(2)
            .any(|p| p == ["-protocol_whitelist", "http,tcp,crypto"]));
        assert!(args.windows(2).any(|p| p == ["-c", "copy"]));
        assert!(args.windows(2).any(|p| p == ["-f", "hls"]));
        assert!(args
            .windows(2)
            .any(|p| p == ["-format_whitelist", "hls,mpegts,mov,aac,mp3"]));
        assert!(args.iter().any(|p| p == "-n"));
        assert!(!args.iter().any(|p| p.contains("Bearer")));
    }
    #[test]
    fn progress_parser_ignores_invalid_numbers_and_clamps_completion() {
        let mut status = Status::default();
        apply_progress(&mut status, "out_time_us=10000000", 20.0);
        apply_progress(&mut status, "total_size=12345", 20.0);
        assert_eq!(status.percent, 50.0);
        assert_eq!(status.bytes, 12345);
        apply_progress(&mut status, "out_time_us=9999999999", 20.0);
        assert_eq!(status.percent, 99.0);
        apply_progress(&mut status, "out_time_us=NaN", 20.0);
        assert!(status.percent.is_finite());
    }
    #[test]
    fn export_manifests_reject_unregistered_and_ambiguous_uris() {
        let prefix = "http://127.0.0.1:1234/cap";
        let good="#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"http://127.0.0.1:1234/cap/seg?p=t&r=k\"\n#EXTINF:3,\nhttp://127.0.0.1:1234/cap/seg?p=t&r=s\n";
        assert!(strict_manifest(good, prefix).is_ok());
        for bad in [
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,URI=http://127.0.0.1:8888/private\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128 URI=http://127.0.0.1:8888/key\n",
            "#EXTM3U\n#EXT-X-MAP:URI=\"http://evil.test/init\"\n",
            "#EXTM3U\n//evil.test/segment\n",
            "#EXTM3U\n#EXT-X-KEY:URI=\"file:///secret\"\n",
            "#EXTM3U\n#EXT-X-MEDIA:URI=\"http://127.0.0.1:1234/other/seg?p=t&r=k\"\n",
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\\\",FOO=\"URI=http://127.0.0.1:8888/private\"\n",
            "#EXTM3U\n#EXT-X-MEDIA:NAME=\"hidden\tvalue\",URI=\"http://127.0.0.1:1234/cap/seg?p=t&r=k\"\n",
        ] {
            assert!(strict_manifest(bad, prefix).is_err(), "{bad}");
        }
    }

    #[test]
    fn finalization_preserves_existing_files_and_removes_partial_output() {
        let dir = std::env::temp_dir().join(format!("hghub-export-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let partial = dir.join("clip.part.mp4");
        let output = dir.join("clip.mp4");
        std::fs::write(&partial, b"new").unwrap();
        std::fs::write(&output, b"existing").unwrap();
        assert!(publish(&partial, &output, Ok(())).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        assert!(!partial.exists());
        std::fs::write(&partial, b"cancelled").unwrap();
        assert!(publish(
            &partial,
            &dir.join("cancelled.mp4"),
            Err("cancelled".into())
        )
        .is_err());
        assert!(!partial.exists());
        assert!(!dir.join("cancelled.mp4").exists());
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[tokio::test]
    #[ignore = "Synthetic FFmpeg export test; requires HGHUB_FFMPEG and HGHUB_TEST_WORK"]
    async fn synthetic_encrypted_hls_remux_and_cancel() {
        let ffmpeg = ffmpeg_path().unwrap();
        let dir = PathBuf::from(std::env::var_os("HGHUB_TEST_WORK").expect("set HGHUB_TEST_WORK"))
            .join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("key.bin");
        std::fs::write(&key, b"0123456789abcdef").unwrap();
        let key_info = dir.join("key-info.txt");
        std::fs::write(&key_info, format!("key.bin\n{}\n", key.display())).unwrap();
        let generation = std::process::Command::new(&ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x72:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100",
                "-t",
                "3",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-g",
                "10",
                "-hls_time",
                "1",
                "-hls_playlist_type",
                "vod",
                "-hls_key_info_file",
                key_info.to_str().unwrap(),
                "-hls_segment_filename",
                dir.join("seg%03d.ts").to_str().unwrap(),
                dir.join("list.m3u8").to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(generation.success());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = dir.clone();
        let manifest = dir.join("list.m3u8");
        let original = std::fs::read_to_string(&manifest).unwrap();
        let local_prefix = format!("http://{address}/cap");
        let mut registry = std::collections::HashMap::from([
            ("manifest-id".to_string(), "list.m3u8".to_string()),
            ("key-id".to_string(), "key.bin".to_string()),
        ]);
        let rewritten = original
            .lines()
            .map(|line| {
                if line.contains("URI=\"key.bin\"") {
                    line.replace(
                        "URI=\"key.bin\"",
                        &format!("URI=\"{local_prefix}/seg?p=t&r=key-id\""),
                    )
                } else if !line.is_empty() && !line.starts_with('#') {
                    let resource = format!("segment-{}", registry.len());
                    registry.insert(resource.clone(), line.to_string());
                    format!("{local_prefix}/seg?p=t&r={resource}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let rewritten = strict_manifest(&rewritten, &local_prefix).unwrap();
        std::fs::write(&manifest, rewritten).unwrap();
        let router = Router::new().route(
            "/cap/{kind}",
            get(move |Query(query): Query<ResourceQuery>| {
                let fixture = fixture.clone();
                let registry = registry.clone();
                async move {
                    let Some(name) = registry.get(&query.r) else {
                        return StatusCode::NOT_FOUND.into_response();
                    };
                    if name.contains(['/', '\\']) {
                        return (StatusCode::BAD_REQUEST, Vec::new()).into_response();
                    }
                    let body = std::fs::read(fixture.join(name)).unwrap_or_default();
                    let kind = if name == "list.m3u8" {
                        "application/vnd.apple.mpegurl"
                    } else {
                        "application/octet-stream"
                    };
                    (StatusCode::OK, [("content-type", kind)], body).into_response()
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let partial = dir.join("result.part.mp4");
        let output = dir.join("result.mp4");
        let result = run_remux(
            &ffmpeg,
            &format!("http://{address}/cap/hls?p=t&r=manifest-id"),
            &partial,
            Default::default(),
            Default::default(),
            3.0,
            || true,
        )
        .await;
        publish(&partial, &output, result).unwrap();
        let probe = ffmpeg.with_file_name(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        let info = std::process::Command::new(probe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration:stream=codec_type",
                "-of",
                "json",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(info.status.success());
        let value: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
        assert!(
            value["format"]["duration"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
                > 2.5
        );
        assert!(value["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["codec_type"] == "video"));
        assert!(value["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["codec_type"] == "audio"));
        let cancel = Arc::new(AtomicBool::new(true));
        let cancelled = dir.join("cancelled.part.mp4");
        let result = run_remux(
            &ffmpeg,
            &format!("http://{address}/cap/hls?p=t&r=manifest-id"),
            &cancelled,
            Default::default(),
            cancel,
            3.0,
            || true,
        )
        .await;
        assert!(publish(&cancelled, &dir.join("cancelled.mp4"), result).is_err());
        assert!(!cancelled.exists());
        assert!(!dir.join("cancelled.mp4").exists());
        server.abort();
    }
}
