# HTTPS 私有试点

这一部署在 Docker/Linux VM 内运行控制面和一个 Worker。操作端通过宿主机回环端口访问，Worker 从独立的内部网络连入 HTTPS。Nginx 只转发 `/v1/control`，AEP 本身监听共享网络命名空间里的回环地址。证书由专用 CA 签发，客户端使用 `--ca-cert` 验证签名和访问名称。

## 一条命令重跑

在 AEP 的 GitHub 克隆根目录执行。需要 Docker、Rust、Bash、jq、OpenSSL、GitHub CLI，以及已登录的 GitHub、Codex 和 Claude 账号。仓库需要公开，并包含 AEP 的 `.github/workflows/pilot.yml`。命令使用当前 GitHub 默认分支的提交作为基线。

```sh
./deploy/pilot/live.sh
```

每次生成新的隔离目标 `test/https-drill-…`、固定 Spec 和专用 HTTPS 站点。真实 Codex 处理一个合成 Rust 任务，独立验收开始后命令暂停 HTTPS 入口，等待 Lease 过期。随后验证旧 Run 发布被拒、Worker 补报事件，以及新 Run 用同一候选 SHA 重跑验收并通过真实 Claude 审查。

发布前需核对 Diff、验收日志和审查报告，再在终端输入提示的 `publish <SHA>`。命令会核对固定 PR head 的 GitHub 协议、桌面和独立验收结果。合并前需核对 CI 并输入 `merge <PR> <SHA>`。确认后会再次检查候选、目标、审查与 CI。直接按 Enter 可退出并保留证据。

运行记录默认位于 Git 公共目录的 `aep/https-drills/`。每次运行有独立目录和 `evidence.tar.gz`，包含 HTTPS 与沙箱核验、断线恢复事件、两次 Run 的证据、共享记忆、候选 Git bundle 和 GitHub 回执。失败或退出时会打包当时可获取的证据。命令清理自己创建的 Docker 资源，保留隔离目标与固定 Spec 分支供复查。

使用 `--output-dir` 可指定归档父目录。命令默认读取用户目录的 Codex 和 Claude 认证文件，macOS 上 Claude 也可使用本机钥匙串。指定 `--codex-auth` 或 `--claude-auth` 可选择已授权的认证文件，`--codex-model` 可选择 Codex 模型。

## 部署

在 AEP 仓库根目录构建控制面、Worker 和出站代理镜像。需要 Docker、Bash、OpenSSL、GitHub CLI 和当前版本的 AEP。

```sh
./deploy/pilot/build.sh
export AEP_PILOT_NAME=aep-pilot
site="$HOME/.local/state/aep-pilot"
./deploy/pilot/site.sh serve "$site"

# 选择一个已存在的隔离目标分支。
./deploy/pilot/site.sh workspace https://github.com/OWNER/REPO.git "$branch" "$git_name" "$git_email"
```

`serve` 保留已有的证书、令牌和命名卷。使用新的私有目录部署新站点。操作端只加载 `operator`，设备容器只接收 `device`。CA 签名私钥保存在操作端的站点目录。该目录权限为 0700。只将 CA **证书**交给客户端。

分别使用试点授权的模型账号登录。把认证文件通过标准输入写入 Worker 的专用用户目录，文件权限为 0600。可先在本机单独准备试点账号的认证文件，再执行以下命令。公开仓库通过匿名 HTTPS 拉取。

```sh
./deploy/pilot/site.sh credential codex < "$codex_auth_json"
./deploy/pilot/site.sh credential claude < "$claude_auth_json"
./deploy/pilot/site.sh worker --detach --codex-model "$model"
```

使用 `worker --once` 可处理一个 Run 后退出。后台模式可通过 Docker 查看日志和重启同名设备。Worker 的业务进程 UID 为 10001。只读根文件系统、独立挂载、空 capabilities 和 `no-new-privileges` 由容器与启动器执行。Worker 的监督进程将设备令牌和 Lease 日志保存在独立的私有目录。应用进程无法读取它们。容器没有宿主机账号目录、SSH agent 或 Docker socket。模型文件仅存放在该试点的 Worker 卷。

从旧镜像升级时，在重启 Worker 前再执行一次 `site.sh workspace`，以初始化监督进程与业务进程共用的取消锁目录。已有工作区和运行记录会保留。

此配置使用容器作为 Codex 的执行沙箱，因为 Docker 的默认 seccomp 与 AppArmor 会阻止内层 bubblewrap 建立命名空间。本部署的 Codex 包装器只在只读容器内的指定 UID、零 capabilities 与 `no-new-privileges` 均成立时接管 AEP 的沙箱参数。代理仅开放配置中的 HTTPS CONNECT 主机，内部网络无直接外部路由。验收使用构建时准备好的 Cargo 缓存并离线运行。[Codex 容器沙箱说明](https://github.com/openai/codex/blob/main/.devcontainer/README.md) · [Docker 内部网络](https://docs.docker.com/reference/cli/docker/network/create/#network-internal-mode---internal)

## 从派发到归档

操作端用隔离目标的 Git 克隆，将已评审的 Spec 固定到 `aep-task/<task>`。在操作端单独加载令牌。

```sh
export AEP_CONTROL_OPERATOR_TOKEN="$(cat "$site/operator")"
aep --repo "$operator_repo" control --url https://127.0.0.1:18443 --ca-cert "$site/ca.pem" submit \
  --ticket "$ticket" --task "$task" --spec "$spec" --target "$branch" --baseline "$baseline"
```

验证断线恢复时，在本地 Run 已写入候选 SHA 后停止该站点的 `$AEP_PILOT_NAME-https` 容器。超过 Lease 时限后恢复入口。检查旧 Run 的补报历史与拒绝发布回执，然后用 `control retry --ticket "$ticket" --reverify` 发起新 Run。新 Run 在同一 SHA 上重新验收和审查。

验收通过后，从 Worker 导出指定 Run 的证据和只含候选引用的 Git bundle。归档导出以只读方式挂载 Worker 卷，传输范围由文件允许列表控制。

```sh
./deploy/pilot/site.sh export "$evidence_dir" "$old_run" "$run"
git -C "$operator_repo" fetch "$evidence_dir/runs.bundle" "refs/aep/runs/$run:refs/aep/runs/$run"
mkdir -p "$operator_repo/.git/aep/runs"
cp -R "$evidence_dir/runs/." "$operator_repo/.git/aep/runs/"
```

操作人核对候选 Diff、验收命令、退出码和 Claude 审查。在操作端执行 `control approve` 和 `control publish`，两次都传入相同的 ticket、Run ID、Spec SHA 和候选 SHA。核对 GitHub PR 的目标、head 和独立验收。检查通过后，用 `gh pr merge --merge --match-head-commit "$candidate"` 合并。归档应包含旧 Run 拒绝发布记录、两轮证据、共享记忆、PR、CI、合并 SHA 和本地目标分支同步结果。[审批参数](pilot.md#派发审查和发布)

站点使用宿主机回环入口。若站点有自己的私网入口或域名，在部署环境用该名称签发证书、配置 HTTPS 代理，并调整操作端与设备的 URL。
