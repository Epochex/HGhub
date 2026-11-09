#!/bin/sh
# @brief 安装已审查的版本目录并只启动 HGhub Web 用户服务。
# @param $1 已提交源码构建的绝对版本目录。
# @return 0 表示服务启动并通过存活检查，否则保留现场供回退。
set -eu
task_base="$HOME/apps/hghub-web"
task_release=$(realpath "$1")
case "$task_release" in "$task_base"/releases/*) ;; *) echo 'release outside application directory' >&2; exit 1 ;; esac
test -x "$task_release/hghub-web"
test -f "$task_release/dist/index.html"
test -f "$task_release/SOURCE.json"
mkdir -p "$task_base/state" "$HOME/.config/systemd/user"
chmod 700 "$task_base/state"
if test -L "$task_base/current"; then
    readlink "$task_base/current" > "$task_base/state/previous-release"
fi
ln -s "$task_release" "$task_base/current.new"
mv -T "$task_base/current.new" "$task_base/current"
cat > "$HOME/.config/systemd/user/hghub-web.service" <<EOF
[Unit]
Description=HGhub personal browser service
After=network-online.target
StartLimitIntervalSec=120
StartLimitBurst=4

[Service]
Type=simple
WorkingDirectory=$task_base/current
ExecStart=$task_base/current/hghub-web
Environment=HGHUB_BIND=127.0.0.1:8093,172.25.244.209:8093,192.168.1.13:8093
Environment=HGHUB_ALLOWED_HOSTS=127.0.0.1:8093,localhost:8093,172.25.244.209:8093,192.168.1.13:8093
Environment=HGHUB_DIST=$task_base/current/dist
Environment=HGHUB_DATA=$task_base/state
Environment=HGHUB_STRICT=1
Environment=HGHUB_PROXY=http://127.0.0.1:7897
Environment=MALLOC_ARENA_MAX=2
Restart=on-failure
RestartSec=5
TimeoutStopSec=20
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=$task_base/state
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
LockPersonality=true
MemoryMax=384M
LimitAS=768M
TasksMax=128
CPUQuota=100%

[Install]
WantedBy=default.target
EOF
systemd-analyze --user verify "$HOME/.config/systemd/user/hghub-web.service"
systemctl --user daemon-reload
systemctl --user enable hghub-web.service
if systemctl --user is-active --quiet hghub-web.service; then
    systemctl --user restart hghub-web.service
else
    systemctl --user start hghub-web.service
fi
task_attempt=0
while test "$task_attempt" -lt 15; do
    if curl --noproxy '*' -fsS http://127.0.0.1:8093/healthz; then exit 0; fi
    task_attempt=$((task_attempt + 1))
    sleep 1
done
systemctl --user status hghub-web.service --no-pager
exit 1
