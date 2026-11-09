# 当前集下载

Windows 原生播放器控制条新增“下载本集”，保存为 MP4，默认目录为系统
“下载/HGhub”。一次一个任务，右下角显示进度、大小和取消；切换页面后仍继续。
不会自动下载下一集。关闭客户端会终止任务；失败或取消不会留下正式 MP4。
完成前使用唯一的 .part.mp4，成功后才发布正式文件，不覆盖已有文件。

MP4 是对当前视频的无损封装，不重新编码。仅保存当前身份有权正常播放的内容；
下载不改变上游访问权限。视频编码保持原样，目标播放器仍需支持其编码。

## 首次安装导出组件

下载发行包旁的“安装下载组件.ps1”，在客户端目录运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\安装下载组件.ps1
```

也可指定目标目录：scripts/install-download-component.ps1 -Destination 客户端目录。
脚本从 FFmpeg 官网列出的 Windows 构建提供方 Gyan 下载 FFmpeg 9.0.2，
校验固定 SHA256，保存其许可证与来源。HGhub 仓库和发行包不包含第三方二进制。
支持 HGHUB_FFMPEG 指定绝对路径，或使用 PATH 中已有的 ffmpeg。

FFmpeg 只连接临时回环代理，输入地址不含上游签名或 CDN 凭据。HLS 清单、
密钥、分片仍经现有 TicketStore 和网络目标检查；账号/线路变化会取消任务。
不会使用浏览器自动化或操作系统前台来下载。
