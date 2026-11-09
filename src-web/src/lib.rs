//! 浏览器适配：复用原生业务与网络策略，提供同源、有限命令的 HTTP 服务。
#[path = "../../src-tauri/src/api.rs"]
mod api;
#[path = "../../src-tauri/src/lines.rs"]
mod lines;
#[path = "../../src-tauri/src/logging.rs"]
mod logging;
#[path = "../../src-tauri/src/media.rs"]
mod media;
#[path = "../../src-tauri/src/net.rs"]
mod net;
#[path = "../../src-tauri/src/network_policy.rs"]
mod network_policy;
mod playback;
#[path = "../../src-tauri/src/playback_log.rs"]
mod playback_log;
#[path = "../../src-tauri/src/session.rs"]
mod session;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tower_http::services::{ServeDir, ServeFile};

const COOKIE: &str = "hghub_session";
const SESSION_TTL: Duration = Duration::from_secs(6 * 3600);

/// @brief 进程提供的 Host 白名单和静态资源目录。
pub struct Config {
    pub allowed_hosts: Vec<String>,
    pub dist: PathBuf,
}
struct BrowserSession {
    api: Arc<api::Api>,
    playback: Arc<playback::TicketStore>,
    touched: Instant,
}
struct WebState {
    hosts: Vec<String>,
    sessions: Mutex<HashMap<String, BrowserSession>>,
    requests: Arc<Semaphore>,
}
struct WebError(StatusCode, String);

impl From<String> for WebError {
    fn from(message: String) -> Self {
        Self(
            if message == "401" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_GATEWAY
            },
            message,
        )
    }
}
impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
fn bad(message: &str) -> WebError {
    WebError(StatusCode::BAD_REQUEST, message.into())
}

/// @brief 构造共享业务核心、同源守卫和静态页面路由。
/// @param config 明确允许的 Host 与前端构建目录。
/// @return 可用于真实监听或隔离测试的路由。
pub fn make_app(config: Config) -> Router {
    let state = Arc::new(WebState {
        hosts: config.allowed_hosts,
        sessions: Mutex::new(HashMap::new()),
        requests: Arc::new(Semaphore::new(12)),
    });
    Router::new()
        .route(
            "/healthz",
            get(|| async { Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/api/invoke", post(invoke))
        .route("/media/{route}", get(media_request))
        .fallback_service(
            ServeDir::new(&config.dist)
                .not_found_service(ServeFile::new(config.dist.join("index.html"))),
        )
        .layer(DefaultBodyLimit::max(16384))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// @brief 初始化只读网络策略和共享线路配置。
/// @param directory 独立运行状态目录，账号状态不在此持久化。
/// @return 目录或严格模式初始化失败时返回错误。
pub fn initialize(directory: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|_| "无法创建运行目录")?;
    net::bind_base(directory);
    if !network_policy::strict_security() {
        return Err("Web 服务必须启用严格安全模式".into());
    }
    Ok(())
}

/// @brief 校验来源并注入隔离会话。@param state 运行状态。@param request 请求。
/// @param next 后续处理器。@return 带安全标头的结果。
async fn guard(State(state): State<Arc<WebState>>, mut request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state
        .hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if origin.to_str().ok() != Some(format!("http://{host}").as_str()) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.uri().path() == "/api/invoke"
        && request
            .headers()
            .get("x-hghub-web")
            .is_none_or(|v| v != "1")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let supplied = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .filter_map(|s| s.trim().split_once('='))
                .find_map(|(k, v)| (k == COOKIE).then_some(v.to_owned()))
        });
    let now = Instant::now();
    let (api, playback, issued) = {
        let mut sessions = state.sessions.lock().unwrap();
        sessions.retain(|_, session| now.duration_since(session.touched) < SESSION_TTL);
        if let Some(session) = supplied.as_ref().and_then(|id| sessions.get_mut(id)) {
            session.touched = now;
            (session.api.clone(), session.playback.clone(), None)
        } else {
            if sessions.len() >= 128 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let id = uuid::Uuid::new_v4().simple().to_string();
            let api = Arc::new(api::Api::new());
            let playback = Arc::new(playback::TicketStore::default());
            sessions.insert(
                id.clone(),
                BrowserSession {
                    api: api.clone(),
                    playback: playback.clone(),
                    touched: now,
                },
            );
            (api, playback, Some(id))
        }
    };
    request.extensions_mut().insert(api);
    request.extensions_mut().insert(playback);
    let mut response = next.run(request).await;
    if let Some(id) = issued {
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict"))
                .unwrap(),
        );
    }
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    command: String,
    #[serde(default)]
    args: Value,
}

/// @brief 限流并执行命令。@param state 运行状态。@param api 会话业务状态。
/// @param command 命令。@return 业务数据或 HTTP 错误。
async fn invoke(
    State(state): State<Arc<WebState>>,
    Extension(api): Extension<Arc<api::Api>>,
    Extension(playback): Extension<Arc<playback::TicketStore>>,
    headers: HeaderMap,
    Json(command): Json<Command>,
) -> Result<Json<Value>, WebError> {
    let _permit = state.requests.try_acquire().map_err(|_| {
        WebError(
            StatusCode::SERVICE_UNAVAILABLE,
            "请求较多，请稍后重试".into(),
        )
    })?;
    let prefix = format!(
        "http://{}/media",
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    );
    let result = tokio::time::timeout(
        Duration::from_secs(80),
        dispatch(&api, &playback, &prefix, command),
    )
    .await
    .map_err(|_| {
        WebError(
            StatusCode::GATEWAY_TIMEOUT,
            "上游响应超时，请稍后重试".into(),
        )
    })??;
    Ok(Json(json!({"data": result})))
}
/// @brief 读取强类型参数。@param args 参数对象。@param name 字段名。@return 参数或错误。
fn field<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, WebError> {
    serde_json::from_value(args.get(name).cloned().ok_or_else(|| bad("缺少必要参数"))?)
        .map_err(|_| bad("参数类型无效"))
}
/// @brief 限制正整数。@param args 参数对象。@param name 字段名。
/// @param max 上限。@return 合法整数或错误。
fn positive(args: &Value, name: &str, max: u64) -> Result<u64, WebError> {
    let number: u64 = field(args, name)?;
    if number == 0 || number > max {
        return Err(bad("数字参数超出范围"));
    }
    Ok(number)
}
/// @brief 限制文本长度。@param args 参数对象。@param name 字段名。
/// @param limit 字节上限。@return 字符串或错误。
fn text(args: &Value, name: &str, limit: usize) -> Result<String, WebError> {
    let value: String = field(args, name)?;
    if value.len() > limit {
        return Err(bad("文本参数过长"));
    }
    Ok(value)
}
/// @brief 分派白名单命令。@param api 会话业务状态。@param request 命令。@return 业务结果。
async fn dispatch(
    api: &api::Api,
    playback: &playback::TicketStore,
    prefix: &str,
    request: Command,
) -> Result<Value, WebError> {
    let a = request.args;
    let page = || positive(&a, "page", 10000).map(|v| v as u32);
    let size = || positive(&a, "size", 50).map(|v| v as u32);
    let id = || positive(&a, "id", i64::MAX as u64).map(|v| v as i64);
    let ep = || positive(&a, "ep", 10000).map(|v| v as u32);
    Ok(match request.command.as_str() {
        "app_boot" => api::app_boot(api).await?,
        "api_feed" => api::api_feed(api, page()?, size()?).await?,
        "api_category" => {
            let category = text(&a, "cate", 128)?;
            let category: String =
                url::form_urlencoded::byte_serialize(category.as_bytes()).collect();
            api::api_category(api, category, page()?, size()?).await?
        }
        "api_rank" => api::api_rank(api, text(&a, "kind", 20)?).await?,
        "api_search" => api::api_search(api, text(&a, "q", 400)?, page()?, size()?).await?,
        "api_posts" => api::api_posts(api, page()?, size()?).await?,
        "api_post" => api::api_post(api, id()?).await?,
        "api_video" => api::api_video(api, id()?).await?,
        "api_play" => playback.prepare(api, id()?, ep()?, prefix).await?,
        "api_comments" => api::api_comments(api, id()?, ep()?, page()?, size()?).await?,
        "api_post_comments" => api::api_post_comments(api, id()?, page()?, size()?).await?,
        "api_login" => {
            api::api_login(api, text(&a, "username", 256)?, text(&a, "password", 512)?).await?
        }
        "api_register" => {
            api::api_register(
                api,
                text(&a, "username", 256)?,
                text(&a, "password", 512)?,
                text(&a, "email", 320)?,
            )
            .await?
        }
        "api_logout" => api::api_logout(api).await?,
        "api_me" => api::api_me(api).await?,
        "api_like" => api::api_like(api, id()?, ep()?, field(&a, "on")?).await?,
        "api_collect" => api::api_collect(api, id()?, field(&a, "on")?).await?,
        "api_follow" => {
            api::api_follow(
                api,
                positive(&a, "uid", i64::MAX as u64)? as i64,
                field(&a, "on")?,
            )
            .await?
        }
        "api_post_like" => api::api_post_like(api, id()?, field(&a, "on")?).await?,
        "api_post_collect" => api::api_post_collect(api, id()?, field(&a, "on")?).await?,
        "api_comment_send" => {
            api::api_comment_send(
                api,
                id()?,
                ep()?,
                text(&a, "text", 4000)?,
                serde_json::from_value(a.get("replyId").cloned().unwrap_or(Value::Null))
                    .map_err(|_| bad("回复参数无效"))?,
            )
            .await?
        }
        "line_state" => api::line_state().await?,
        "line_mirrors" => api::line_mirrors().await?,
        "line_set" => json!(api::line_set(api, text(&a, "url", 2048)?).await?),
        "line_probe" => {
            let urls: Vec<String> = field(&a, "urls")?;
            if urls.len() > 8 || urls.iter().any(|url| url.len() > 2048) {
                return Err(bad("线路数量或地址过长"));
            }
            api::line_probe(urls).await?
        }
        "check_update" => api::check_update().await?,
        "cache_clear" => {
            api::cache_clear().await?;
            Value::Null
        }
        "security_set" => {
            if !field::<bool>(&a, "strict")? {
                return Err(bad("网页服务固定启用严格安全模式"));
            }
            json!(true)
        }
        "debug_log" | "playback_log_write" => Value::Null,
        "playback_log_status" => json!({"enabled": false, "path": null, "error": null}),
        "playback_log_set" => {
            if field::<bool>(&a, "enabled")? {
                return Err(bad("网页版请使用浏览器开发者工具查看播放诊断"));
            }
            json!({"enabled": false, "path": null, "error": null})
        }
        "playback_log_read" => json!(""),
        "net_diag" => json!({"mode": "web", "strict_security": true, "base": net::base()}),
        _ => return Err(bad("不支持的网页命令")),
    })
}

/// @brief 校验并转发媒体。@param route 媒体路由。@param query 目标参数。
/// @param headers 请求标头。@return 同源播放列表、封面或流式响应。
async fn media_request(
    Extension(api): Extension<Arc<api::Api>>,
    Extension(playback): Extension<Arc<playback::TicketStore>>,
    Path(route): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    if !["img", "hls", "seg"].contains(&route.as_str()) {
        return Err(WebError(StatusCode::NOT_FOUND, "媒体路径无效".into()));
    }
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    if range.is_some_and(|value| {
        value.len() > 100
            || !value.starts_with("bytes=")
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || b"bytes=,- ".contains(&b))
    }) {
        return Err(bad("Range 参数无效"));
    }
    if query.contains_key("p") || query.contains_key("r") {
        let ticket = query
            .get("p")
            .filter(|v| v.len() == 32)
            .ok_or_else(|| bad("播放票据无效"))?;
        let resource = query
            .get("r")
            .filter(|v| v.len() == 32)
            .ok_or_else(|| bad("播放资源无效"))?;
        return playback
            .serve(&api, ticket, resource, range)
            .await
            .map_err(|error| WebError(StatusCode::GONE, error));
    }
    let target = query
        .get("u")
        .filter(|value| value.len() <= 8192)
        .ok_or_else(|| bad("媒体参数无效"))?;
    network_policy::parse_target(target)
        .map_err(|_| WebError(StatusCode::FORBIDDEN, "禁止访问该媒体地址".into()))?;
    if route == "seg" && !target.split('?').next().unwrap_or("").ends_with(".m3u8") {
        let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
        if range.is_some_and(|value| {
            value.len() > 100
                || !value.starts_with("bytes=")
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b"bytes=,- ".contains(&b))
        }) {
            return Err(bad("Range 参数无效"));
        }
        let upstream = net::get_with_range(
            target,
            Duration::from_secs(40),
            None,
            Some(&format!("{}/", net::base())),
            range,
        )
        .await?;
        if upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_none_or(|s| !s.to_ascii_lowercase().contains("mpegurl"))
        {
            let mut builder = http::Response::builder().status(upstream.status());
            for name in [
                header::CONTENT_TYPE,
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::ACCEPT_RANGES,
            ] {
                if let Some(value) = upstream.headers().get(&name) {
                    builder = builder.header(name, value);
                }
            }
            return builder
                .body(Body::from_stream(upstream.bytes_stream()))
                .map_err(|_| WebError(StatusCode::BAD_GATEWAY, "媒体响应无效".into()));
        }
    }
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("u", target)
        .finish();
    Ok(media::handle(&format!("/{route}"), &query, "/media")
        .await
        .map(Body::from))
}
