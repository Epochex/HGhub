# HGhub 树莓派网页适配

在原始 HGhub 客户端上增加独立 HTTP 服务。前端保留原界面和本地收藏、历史；
Rust 侧复用业务命令、线路、封面处理、HLS 重写和逐跳网络检查。

## 架构和边界

手机/电脑浏览器 → 同源 HTTP 服务（静态页面、命令白名单、媒体流） → 上游站点。
不需要数据库；视频按需流式转发，不提供下载或后台采集。
服务绑定明确的 IPv4 私网地址，Host 白名单应与访问地址一致。
默认只监听 127.0.0.1:8093，不应配置公网转发。

每个浏览器通过 HttpOnly、SameSite=Strict 随机会话 Cookie 隔离上游登录；
账号状态仅保存在服务内存，空闲六小时过期，服务重启后需要重新登录。
收藏、观看和搜索历史保存在各浏览器本地，不在设备间同步。
线路和封面/HLS 缓存由服务共享。切换线路会影响其他访问者。
Web 服务固定启用严格网络策略，浏览器不能关闭；诊断日志不集中落盘。

## 构建和验证

需要 Node 20.19+、pnpm 10、Rust 1.90+、Linux C 编译工具链。

```sh
pnpm install --frozen-lockfile
node scripts/test-web.mjs
pnpm build
cargo test --manifest-path src-web/Cargo.toml --locked -j 1 -- --test-threads=1
cargo build --manifest-path src-web/Cargo.toml --locked --release -j 1
```

src-web/Cargo.lock 固定 Web 服务依赖，src-tauri 继续保留原生构建入口。
Web 服务不依赖 GTK/WebKit，不需要图形桌面。

## 运行

```sh
HGHUB_BIND=127.0.0.1:8093 \
HGHUB_ALLOWED_HOSTS=127.0.0.1:8093,localhost:8093 \
HGHUB_DIST="$PWD/dist" HGHUB_DATA="$HOME/.local/state/hghub-web" \
HGHUB_STRICT=1 src-web/target/release/hghub-web
```

多个监听地址、允许的 Host 使用逗号分隔。可使用原生支持的 HGHUB_PROXY
配置固定 HTTP 代理；不修改机器的系统代理或现有服务。

生产使用独立 systemd 用户服务，入口指向已提交源码构建的版本目录。
运行目录和日志必须在 Git 外，部署时记录源码 commit、二进制及前端 SHA256。
服务使用 Restart=on-failure、资源上限、NoNewPrivileges 和只读系统沙箱。
当前树莓派内核未提供 memory cgroup 控制器，MemoryMax 属于备用配置；
另用 LimitAS=768M 限制进程虚拟地址空间，MALLOC_ARENA_MAX=2 限制分配器 arena。
没有修改启动参数或重启树莓派。
更换 current 符号链接后仅重启 hghub-web；回退指向保留的上一版本。

## 接口

- GET /healthz：服务存活状态和版本，不代表上游可用。
- POST /api/invoke：JSON {command,args}，必须携带 X-HGhub-Web: 1。
  成功返回 {data}，错误返回 {error} 和相应 HTTP 状态。
- GET /media/img|hls|seg?u=：兼容原生封面和媒体地址，保持逐跳 URL/DNS 检查；
  分片和 MP4 流式转发，并转发 Range、Content-Range 和 Accept-Ranges。

上游站点的可用性和内容不由本项目保证。发布时保留原 LICENSE 和 Required Notice；
本适配继续采用 PolyForm Noncommercial 1.0.0，仅供个人非商业使用。

## 2026-11-09 上游播放接口适配

上游原 GET 播放接口仍返回成功状态，但不再提供地址。当前按网页播放器的流程，
为当前身份和集数 POST 申请播放凭据，读取 streams.main 和 CDN 标头配置。
参考 [issue #31 中 zxsos 提供的修复](https://github.com/zxsos/HGhub/commit/977a40decdddf7dac29537b8b83862d44c6dfece)，
并在网页服务中补充以下处理：

- 浏览器只得到 /media/hls?p=票据&r=资源 形式的本地地址；签名地址、CDN 凭据
  仅存于当前浏览器对应的服务端内存，未写入日志、配置或浏览器历史。
- 每浏览器最多保留八组票据，三十分钟失效；按当前账号及线路校验，禁止跨会话复用。
- HLS 主/子清单、分片、初始化片段和密钥都经本地资源登记；保留原有内容安全过滤。
  只有可信 CDN 原点收到对应标头，跨源重定向不转发这些凭据。
- 清单保留有界缓存，媒体流和 Range 继续透传；网络异常或票据过期最多自动重新
  取址一次，保留续播位置，显式“重试”也重新申请。切回页面/从后台恢复时更新凭据。
- 真实浏览器验收只保存解码、播放时间和 HTTP 状态等技术数据，不保存画面或媒体地址。

临时树莓派实例已验证真实清单、密钥、分片返回成功，桌面/手机浏览器布局下均已解码
并完成进度跳转。正式部署记录以 current/SOURCE.json 和 state/deployment-result.json 为准。
