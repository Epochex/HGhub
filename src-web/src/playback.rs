//! 网页播放凭据：只在对应浏览器的服务端内存中保存。
use crate::{api::Api, media, net, network_policy};
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{stream, StreamExt};
use http::{HeaderMap, HeaderName};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

const TICKET_LIFETIME: Duration = Duration::from_secs(30 * 60);
const MAX_MANIFEST: usize = 1024 * 1024;

#[derive(Clone, PartialEq)]
pub(crate) struct Identity {
    pub origin: String,
    pub token: Option<String>,
}

impl Identity {
    /// @brief 获取当前线路与身份快照。@param api 浏览器业务状态。@return 内存身份快照。
    pub fn current(api: &Api) -> Self {
        let origin = net::base();
        Self {
            token: api.session.token(&origin),
            origin,
        }
    }
}

pub(crate) struct Resource {
    pub url: String,
    pub headers: HeaderMap,
}

struct Cached {
    body: String,
    created: Instant,
    vod: bool,
}
struct Grant {
    id: String,
    identity: Identity,
    created: Instant,
    prefix: String,
    origins: HashSet<String>,
    headers: HeaderMap,
    manifests: bool,
    segments: bool,
    resources: HashMap<String, String>,
    reverse: HashMap<String, String>,
    cache: HashMap<String, Cached>,
}

impl Grant {
    fn resource(&mut self, target: &str, route: &str) -> Result<String, String> {
        let parsed = network_policy::parse_target(target)?;
        if parsed.scheme() != "https" || target.len() > 8192 {
            return Err("媒体地址无效".into());
        }
        let resource = if let Some(id) = self.reverse.get(target) {
            id.clone()
        } else {
            if self.resources.len() >= 4096 {
                return Err("播放清单资源过多".into());
            }
            let id = uuid::Uuid::new_v4().simple().to_string();
            self.resources.insert(id.clone(), target.to_owned());
            self.reverse.insert(target.to_owned(), id.clone());
            id
        };
        Ok(format!(
            "{}/{route}?p={}&r={resource}",
            self.prefix, self.id
        ))
    }
}

pub(crate) struct TicketStore {
    grants: Mutex<VecDeque<Grant>>,
    transfers: Arc<Semaphore>,
}
impl Default for TicketStore {
    fn default() -> Self {
        Self {
            grants: Mutex::new(VecDeque::new()),
            transfers: Arc::new(Semaphore::new(6)),
        }
    }
}

impl TicketStore {
    /// @brief 将新取址响应转成本地票据，拒绝身份错配和不安全 CDN 标头。
    /// @param payload 上游响应。@param id 期望编号。@param ep 期望集数。
    /// @param identity 身份快照。@param prefix 已通过 Host 校验的本地地址。
    /// @return 兼容前端的播放信息，不含上游地址或 CDN 凭据。
    pub fn install(
        &self,
        payload: &Value,
        id: i64,
        ep: u32,
        identity: Identity,
        prefix: &str,
    ) -> Result<Value, String> {
        let matches_id = payload["id"].as_i64() == Some(id)
            || payload["id"].as_str().is_some_and(|s| s == id.to_string());
        if !matches_id
            || payload["ep"].as_u64() != Some(ep as u64)
            || payload["manifest_method"] != "GET"
        {
            return Err("播放凭据与当前集数不匹配".into());
        }
        let main = payload["streams"]["main"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("上游暂未提供播放地址")?;
        let hevc = payload["streams"]["hevc"]
            .as_str()
            .filter(|s| !s.is_empty());
        let manifests = payload["cdn"]["m3u8Enabled"].as_bool().unwrap_or(false);
        let segments = payload["cdn"]["otherEnabled"].as_bool().unwrap_or(false);
        let mut headers = HeaderMap::new();
        if manifests || segments {
            let name = payload["cdn"]["authKey"].as_str().ok_or("CDN 凭据缺失")?;
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "CDN 标头无效")?;
            if !name.as_str().starts_with("x-")
                || name.as_str() == "x-cdn-app"
                || name.as_str().starts_with("x-forwarded-")
            {
                return Err("CDN 标头不允许".into());
            }
            let value = payload["cdn"]["authValue"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 1024)
                .ok_or("CDN 凭据无效")?;
            let app = payload["cdn"]["app"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .ok_or("CDN 应用标识无效")?;
            let mut auth = HeaderValue::from_str(value).map_err(|_| "CDN 标头无效")?;
            auth.set_sensitive(true);
            headers.insert(name, auth);
            headers.insert(
                "x-cdn-app",
                HeaderValue::from_str(app).map_err(|_| "CDN 标头无效")?,
            );
        }
        let mut origins = HashSet::new();
        for url in std::iter::once(main).chain(hevc) {
            let parsed = network_policy::parse_target(url)?;
            if parsed.scheme() != "https" {
                return Err("媒体地址必须使用 HTTPS".into());
            }
            origins.insert(parsed.origin().ascii_serialization());
        }
        let mut grant = Grant {
            id: uuid::Uuid::new_v4().simple().to_string(),
            identity,
            created: Instant::now(),
            prefix: prefix.into(),
            origins,
            headers,
            manifests,
            segments,
            resources: HashMap::new(),
            reverse: HashMap::new(),
            cache: HashMap::new(),
        };
        let video_url = grant.resource(main, "hls")?;
        let video_url_hevc = hevc
            .map(|url| grant.resource(url, "hls"))
            .transpose()?
            .unwrap_or_default();
        let mut grants = self.grants.lock().map_err(|_| "播放会话忙")?;
        grants.retain(|grant| grant.created.elapsed() < TICKET_LIFETIME);
        while grants.len() >= 8 {
            grants.pop_front();
        }
        grants.push_back(grant);
        Ok(
            json!({"id":id,"ep_num":ep,"duration":0,"title":"","video_url":video_url,"video_url_hevc":video_url_hevc}),
        )
    }

    /// @brief 解析当前浏览器已登记的媒体资源，并按 CDN 原点限制标头。
    /// @param ticket 票据。@param resource 资源编号。@param identity 当前身份。
    /// @return 只在服务端使用的请求信息。
    pub fn resolve(
        &self,
        ticket: &str,
        resource: &str,
        identity: &Identity,
    ) -> Result<Resource, String> {
        let grants = self.grants.lock().map_err(|_| "播放会话忙")?;
        let grant = grants
            .iter()
            .find(|g| {
                g.id == ticket && g.identity == *identity && g.created.elapsed() < TICKET_LIFETIME
            })
            .ok_or("播放凭据已过期，请重试")?;
        let url = grant
            .resources
            .get(resource)
            .ok_or("播放资源未登记")?
            .clone();
        let parsed = network_policy::parse_target(&url)?;
        let manifest = parsed.path().to_ascii_lowercase().ends_with(".m3u8");
        let use_headers = grant
            .origins
            .contains(&parsed.origin().ascii_serialization())
            && if manifest {
                grant.manifests
            } else {
                grant.segments
            };
        Ok(Resource {
            url,
            headers: if use_headers {
                grant.headers.clone()
            } else {
                HeaderMap::new()
            },
        })
    }

    /// @brief 改写清单中的每个媒体 URI 并登记资源，拒绝内网及未知协议。
    /// @param ticket 票据。@param resource 父清单编号。@param text 清单正文。
    /// @param identity 当前身份。@return 只包含本地随机票据的播放清单。
    pub fn rewrite(
        &self,
        ticket: &str,
        resource: &str,
        text: &str,
        identity: &Identity,
    ) -> Result<String, String> {
        if !text.trim_start_matches('\u{feff}').starts_with("#EXTM3U") || text.len() > MAX_MANIFEST
        {
            return Err("播放清单格式无效".into());
        }
        let parent = self.resolve(ticket, resource, identity)?.url;
        let mut grants = self.grants.lock().map_err(|_| "播放会话忙")?;
        let grant = grants
            .iter_mut()
            .find(|g| g.id == ticket)
            .ok_or("播放凭据已过期")?;
        let mut failed = false;
        let output = media::rewrite_playlist_with(&parent, text, |target| {
            match grant.resource(target, "seg") {
                Ok(url) => url,
                Err(_) => {
                    failed = true;
                    String::new()
                }
            }
        });
        if failed {
            return Err("播放清单含不允许的媒体地址".into());
        }
        if grant.cache.len() >= 8 {
            grant.cache.clear();
        }
        grant.cache.insert(
            resource.into(),
            Cached {
                body: output.clone(),
                created: Instant::now(),
                vod: text.contains("#EXT-X-ENDLIST"),
            },
        );
        Ok(output)
    }

    /// @brief 从新 POST 接口申请一次当前集的播放凭据，不持久化或共享账号信息。
    /// @param api 当前浏览器业务状态。@param id 编号。@param ep 集数。
    /// @param prefix 本地媒体前缀。@return 兼容旧界面的本地播放信息。
    pub async fn prepare(
        &self,
        api: &Api,
        id: i64,
        ep: u32,
        prefix: &str,
    ) -> Result<Value, String> {
        let identity = Identity::current(api);
        let payload = crate::api::api_playback_payload(api, id, ep).await.map_err(|error| {
            if error == "401" {
                error
            } else {
                "获取播放凭据失败，请稍后重试".into()
            }
        })?;
        if Identity::current(api) != identity {
            return Err("取址期间登录或线路已变化，请重试".into());
        }
        self.install(&payload, id, ep, identity, prefix)
    }

    /// @brief 转发当前票据的媒体，保留 Range 并将清单中的资源换成本地票据。
    /// @param api 当前业务状态。@param ticket 票据。@param resource 资源。
    /// @param range 已校验 Range。@return 媒体响应或可读的失败原因。
    pub async fn serve(
        &self,
        api: &Api,
        ticket: &str,
        resource: &str,
        range: Option<&str>,
    ) -> Result<Response, String> {
        let identity = Identity::current(api);
        let request = self.resolve(ticket, resource, &identity)?;
        {
            let grants = self.grants.lock().map_err(|_| "播放会话忙")?;
            if let Some(cached) = grants
                .iter()
                .find(|g| g.id == ticket)
                .and_then(|g| g.cache.get(resource))
                .filter(|c| c.vod || c.created.elapsed() < Duration::from_secs(3))
            {
                return Ok((
                    [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
                    cached.body.clone(),
                )
                    .into_response());
            }
        }
        let permit = self
            .transfers
            .clone()
            .try_acquire_owned()
            .map_err(|_| "媒体请求较多，请稍后重试")?;
        let upstream = net::get_with_headers(
            &request.url,
            Duration::from_secs(40),
            None,
            Some(&format!("{}/", identity.origin)),
            range,
            &request.headers,
        )
        .await
        .map_err(|_| "媒体连接失败，请重试")?;
        if Identity::current(api) != identity {
            return Err("播放身份已变化，请重试".into());
        }
        let status = upstream.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err("播放凭据已过期，请重试".into());
        }
        if !status.is_success() && status != StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(format!("媒体源暂不可用（HTTP {}）", status.as_u16()));
        }
        let manifest_hint = upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.to_ascii_lowercase().contains("mpegurl"));
        let final_url = upstream.url().clone();
        let mut builder = http::Response::builder().status(status);
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
        let mut chunks = upstream.bytes_stream();
        let mut first = Vec::new();
        while first.len() < 16 {
            match chunks.next().await {
                Some(Ok(bytes)) => first.extend_from_slice(&bytes),
                Some(Err(_)) => return Err("媒体读取失败".into()),
                None => break,
            }
        }
        if manifest_hint
            || first.starts_with(b"#EXTM3U")
            || first.starts_with(b"\xef\xbb\xbf#EXTM3U")
        {
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|_| "播放清单读取失败")?;
                if first.len() + chunk.len() > MAX_MANIFEST {
                    return Err("播放清单过大".into());
                }
                first.extend_from_slice(&chunk);
            }
            let text = std::str::from_utf8(&first).map_err(|_| "播放清单编码无效")?;
            // 重定向的最终 URL 决定相对分片地址；跨源跳转不扩大 CDN 凭据适用范围。
            if final_url.as_str() != request.url {
                let mut grants = self.grants.lock().map_err(|_| "播放会话忙")?;
                if let Some(grant) = grants.iter_mut().find(|g| g.id == ticket) {
                    grant
                        .resources
                        .insert(resource.into(), final_url.to_string());
                }
            }
            let body = self.rewrite(ticket, resource, text, &identity)?;
            return Ok((
                [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
                body,
            )
                .into_response());
        }
        let remaining = chunks.map(move |chunk| {
            let _keep = &permit;
            chunk.map_err(|_| std::io::Error::other("media stream failed"))
        });
        builder
            .body(Body::from_stream(
                stream::once(async move { Ok::<_, std::io::Error>(Bytes::from(first)) })
                    .chain(remaining),
            ))
            .map_err(|_| "媒体响应无效".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> Identity {
        Identity {
            origin: "https://example.test".into(),
            token: Some("dummy-user-token".into()),
        }
    }
    fn payload() -> Value {
        json!({"id":123,"ep":2,"manifest_method":"GET","timestamp":1,
            "streams":{"main":"https://cdn.example.test/main.m3u8?auth_key=dummy-signature","hevc":""},
            "cdn":{"m3u8Enabled":true,"otherEnabled":false,"authKey":"x-test-auth","authValue":"dummy-cdn-token","app":"dummy-app"}})
    }
    fn parts(url: &str) -> (String, String) {
        let parsed = url::Url::parse(url).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        (query["p"].clone(), query["r"].clone())
    }

    #[test]
    fn new_payload_becomes_an_opaque_browser_local_url() {
        let store = TicketStore::default();
        let output = store
            .install(
                &payload(),
                123,
                2,
                identity(),
                "http://localhost:8093/media",
            )
            .unwrap();
        let source = output["video_url"].as_str().unwrap();
        assert!(source.starts_with("http://localhost:8093/media/hls?"));
        assert!(!output.to_string().contains("dummy-signature"));
        assert!(!output.to_string().contains("dummy-cdn-token"));
        assert_eq!(output["ep_num"], 2);
        let (ticket, resource) = parts(source);
        let resolved = store.resolve(&ticket, &resource, &identity()).unwrap();
        assert_eq!(resolved.headers["x-test-auth"], "dummy-cdn-token");
        assert_eq!(resolved.headers["x-cdn-app"], "dummy-app");
    }

    #[test]
    fn expired_tickets_and_evicted_tickets_cannot_be_reused() {
        let store = TicketStore::default();
        let output = store
            .install(
                &payload(),
                123,
                2,
                identity(),
                "http://localhost:8093/media",
            )
            .unwrap();
        let (ticket, resource) = parts(output["video_url"].as_str().unwrap());
        store.grants.lock().unwrap()[0].created = Instant::now() - TICKET_LIFETIME;
        assert!(store.resolve(&ticket, &resource, &identity()).is_err());
        let first = store
            .install(
                &payload(),
                123,
                2,
                identity(),
                "http://localhost:8093/media",
            )
            .unwrap();
        let (ticket, resource) = parts(first["video_url"].as_str().unwrap());
        for _ in 0..8 {
            store
                .install(
                    &payload(),
                    123,
                    2,
                    identity(),
                    "http://localhost:8093/media",
                )
                .unwrap();
        }
        assert!(store.resolve(&ticket, &resource, &identity()).is_err());
        assert_eq!(store.grants.lock().unwrap().len(), 8);
    }

    #[test]
    fn native_protocol_urls_remain_opaque_and_line_bound() {
        let store = TicketStore::default();
        let output = store.install(&payload(), 123, 2, identity(), "http://hg.localhost").unwrap();
        let source = output["video_url"].as_str().unwrap();
        assert!(source.starts_with("http://hg.localhost/hls?p="));
        assert!(!source.contains("dummy-signature"));
        let (ticket, resource) = parts(source);
        let mut changed = identity();
        changed.origin = "https://other.example.test".into();
        assert!(store.resolve(&ticket, &resource, &changed).is_err());
        let rewritten = store.rewrite(&ticket, &resource, "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:6,\npart.ts\n", &identity()).unwrap();
        assert!(!rewritten.contains("cdn.example.test"));
        assert!(rewritten.contains("http://hg.localhost/seg?p="));
    }

    #[test]
    fn mismatched_identity_and_forged_resources_are_denied() {
        let store = TicketStore::default();
        let output = store
            .install(
                &payload(),
                123,
                2,
                identity(),
                "http://localhost:8093/media",
            )
            .unwrap();
        let (ticket, resource) = parts(output["video_url"].as_str().unwrap());
        assert!(store
            .resolve(&ticket, "dummy-forged-resource", &identity())
            .is_err());
        let mut changed = identity();
        changed.token = Some("dummy-other-user".into());
        assert!(store.resolve(&ticket, &resource, &changed).is_err());
        assert!(TicketStore::default()
            .resolve(&ticket, &resource, &identity())
            .is_err());
    }

    #[test]
    fn validates_requested_episode_and_rejects_private_streams_and_header_injection() {
        for mutate in [0, 1, 2, 3] {
            let mut value = payload();
            match mutate {
                0 => value["ep"] = json!(3),
                1 => value["streams"]["main"] = json!("http://127.0.0.1/secret"),
                2 => value["cdn"]["authKey"] = json!("Cookie"),
                _ => value["cdn"]["authValue"] = json!("dummy-token\r\nCookie: dummy-cookie"),
            }
            assert!(TicketStore::default()
                .install(&value, 123, 2, identity(), "http://localhost:8093/media")
                .is_err());
        }
    }

    #[test]
    fn nested_playlist_and_key_urls_stay_local_and_credentials_stay_on_the_cdn() {
        let store = TicketStore::default();
        let output = store
            .install(
                &payload(),
                123,
                2,
                identity(),
                "http://localhost:8093/media",
            )
            .unwrap();
        let (ticket, resource) = parts(output["video_url"].as_str().unwrap());
        let manifest="#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example.test/test.key\"\nchild.m3u8\nhttps://segments.example.test/test.ts\n";
        let rewritten = store
            .rewrite(&ticket, &resource, manifest, &identity())
            .unwrap();
        assert!(!rewritten.contains("cdn.example.test"));
        assert!(!rewritten.contains("keys.example.test"));
        let child = rewritten
            .lines()
            .find(|s| s.contains("/seg?") && !s.starts_with('#'))
            .unwrap();
        let (t, r) = parts(child);
        let resolved = store.resolve(&t, &r, &identity()).unwrap();
        assert!(resolved
            .url
            .starts_with("https://cdn.example.test/child.m3u8"));
        assert_eq!(resolved.headers["x-test-auth"], "dummy-cdn-token");
        let segment = rewritten
            .lines()
            .filter(|s| !s.starts_with('#'))
            .last()
            .unwrap();
        let (t, r) = parts(segment);
        assert!(store
            .resolve(&t, &r, &identity())
            .unwrap()
            .headers
            .is_empty());
        assert!(store
            .rewrite(
                &ticket,
                &resource,
                "#EXTM3U\nhttp://127.0.0.1/private\n",
                &identity()
            )
            .is_err());
    }
}
