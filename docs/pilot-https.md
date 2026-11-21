# HTTPS 私有试点

这一部署在 Docker/Linux VM 内运行控制面和一个 Worker。操作端通过宿主机回环端口访问，Worker 从独立的内部网络连入 HTTPS。Nginx 转发 `/v1/control` 和 `/v1/evidence/`，AEP 本身监听共享网络命名空间里的回环地址。证书由专用 CA 签发，客户端使用 `--ca-cert` 验证签名和访问名称。

## 在 Studio 运行

在 Studio 打开包含 `deploy/pilot/live.sh` 的 AEP 源码仓库，选择侧栏的 **HTTPS 试点**，点击 **启动隔离试点**。Studio 使用下方命令所需的同一套 Docker 环境和登录凭证。当前阶段显示 HTTPS 停止、Lease 过期后的重连与事件回放，以及用同一候选重新验收和审查的进度。

发布前在 **日志与证据** 查看候选 Diff、验收日志和 Claude 审查，核对后点击发布确认。创建 PR 后可以在页面查看 GitHub 检查进度。全部检查通过后，核对候选 SHA、目标与 CI，再点击合并确认。可在任一确认步骤选择 **停止并归档**。关闭 Studio 会关闭确认通道，脚本到达下一个确认点时会停止并保留证据。

完成或失败后可在同一页面查看运行回执、失败原因和证据目录。Studio 会话保存于 Git 公共目录的 `aep/https-studio/`，其中包含启动日志和该次试点生成的 `evidence.tar.gz`。重新打开同一个仓库和页面可查看最近一次会话。环境检查失败时保留启动日志。

可用 Studio 的离屏调试入口复跑并保存界面截图。调试入口需要终端操作人确认，并通过 Studio 控件提交确认。

```sh
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --live-https-pilot --output-dir desktop/target/live-https-ui
```

### 创建与派发真实任务

在 Studio 的 **HTTPS 试点 → 真实任务** 选择本地 GitHub 克隆和目标分支。填写 HTTPS 控制面的 origin、操作员令牌文件路径，以及私有 CA 的证书路径。使用系统信任的证书时可将 CA 留空。

选择已有任务可以直接派发。创建任务时先点 **创建 HTTPS 任务…**，填写任务 ID、Spec、允许修改的文件和验收命令，再点 **创建并派发给远程 Worker**。文件路径每行一个。验收命令可以是 JSON 参数数组，也可以是带 `check`、`criteria` 和 `checks` 的对象。

Studio 会在私有工作区以选定的目标 SHA 创建 Spec 提交，固定到 `aep-task/<任务 ID>`，再通过 HTTPS 派发给 Worker。归档记录实际提交的 Spec、验收清单及派发回执。若 GitHub 已保存 Spec 而 HTTPS 派发失败，可以在原任务记录重试派发。

Spec SHA 与目标 SHA 在派发前会重新核对。目标提交必须是 Spec 的祖先，且两者之间只包含任务目录中的变更。

此流程连接已经部署的控制面。每个控制面对应一个项目，Worker 需要能匿名拉取所选的公开 GitHub 仓库。目标分支应包含 AEP 的独立验收工作流。使用更新后的控制面、Nginx 配置与 Worker，在远程 Worker 侧按部署步骤准备镜像、工作区和模型凭证，然后启动 `site.sh worker --detach --upload-evidence`。私网部署需要开放 `/v1/control` 和 `/v1/evidence/` 的 HTTPS 路由。

Studio 会等待控制面重新连接并收齐旧 Run 的终止事件。Lease 过期时，有候选的 Run 会沿用同一 SHA 重新验收和审查，尚未生成候选的 Run 会重新派发实现。Worker 使用设备令牌和该 Run 的 Fence 上传固定范围的证据。操作员用其令牌读取对应工单的证据。单次传输上限为 24 MiB，控制面保留收到的原始证据以便重复下载。

发布和合并各需一次 Studio 确认。CI 核对结果、恢复事件、候选、验收、审查和合并回执存入本地会话归档。合并后同步该会话的操作端克隆，并清理已发布的候选分支。

### 多 Worker 并行

同一 HTTPS 控制面可注册多台 Worker，每台同时持有一条 Run。Studio 的 **HTTPS 任务队列** 显示各 Worker 的心跳状态、任务与当前阶段，以及排队位置。心跳超过 Lease 时限时，该 Worker 显示离线，其他在线 Worker 可继续运行或领取队列中的可执行任务。断线 Run 已生成候选时，重新验收会留给原 Worker，沿用同一候选 SHA。

每台 Worker 使用独立的令牌、安装身份、仓库和证据目录。原站点的设备令牌仍对应默认的 `<站点>-worker`。新增 Worker 时，在可信操作机的访问目录中准备权限为 600 的 `workers.json`，对象的键为完整 Worker ID，例如 `<站点>-worker-two`，值为单独生成的随机令牌。文件应列出该站点所有额外 Worker。部署顺序如下。

1. 以默认槽调用 `site.sh worker-config <访问目录>`，在空闲站点升级控制面。
2. 设置 `AEP_PILOT_WORKER_SLOT=two`，调用 `site.sh worker-keys <访问目录>`，再按下方部署步骤为该槽准备仓库和模型凭证。
3. 用同一槽运行 `site.sh worker --detach --upload-evidence`。Studio 会在首个心跳后显示新 Worker。

离屏复跑可用 `ui_debug --live-https-workers --output-dir <目录>`。先连接这个空闲站点，准备两条已固定 Spec 的任务和各自的 `test/` 目标分支。将 `AEP_STUDIO_LIVE_WORKERS_PLAN` 指向含 `site`、`second_slot`、`recover`、`steady` 的 JSON 文件，两条任务各包含 `task` 和 `target`。Worker 验收命令应以部署手册中的容器验收门闩等待，保证有足够时间断开指定 Worker。此演练只断开目标 Worker 的 Docker 私网，发布和合并仍各自确认。断线演练完成后若终端退出，设置 `AEP_STUDIO_LIVE_WORKERS_RESUME=1` 可继续两个已保存的任务。

### 队列管理

在 **HTTPS 试点 → 真实任务** 使用现有连接，或填写站点后点 **连接队列**。**暂停派发** 保留等待队列的顺序并阻止发出新 Lease。已在运行的 Worker 可以继续上报。可对等待的任务上移或下移，控制面会保存顺序，关闭 Studio 或重启控制面后都可恢复。选择 **恢复派发** 让空闲 Worker 按新顺序领取。

取消需要对当前 Run 确认。等待任务会直接结束并保存调度回执。已运行任务先显示等待 Worker 确认停止，收到进程退出回执并下载证据后可在任务详情打开归档。控制面在取消确认前保留当前 Worker 的位置，避免提前派发下一项。此功能需要支持队列管理的控制面，沿用已启用证据上传的 Worker。

可用 `ui_debug --live-https-queue --output-dir <目录>` 在空闲站点复跑。先在 Studio 保存该站点连接，准备两个已固定 Spec 的任务和各自的 `test/` 目标分支，再将 `AEP_STUDIO_LIVE_QUEUE_PLAN` 指向 JSON 文件。文件的 `cancel` 和 `deliver` 各包含 `task` 和 `target`。取消用的任务应在 Worker 验收期间留出足够时间操作。验收会打开两次 Studio，保留取消和合并两份归档，并通过终端确认发布和合并。若取消已归档而验收程序中断，可设置 `AEP_STUDIO_LIVE_QUEUE_RESUME=1` 后沿用原计划继续交付任务。

### 查看、继续与重试任务

在同一个仓库打开 **HTTPS 试点**，从 **仓库的 HTTPS 任务** 选一条记录。列表显示目标分支、阶段和失败摘要，详情保留日志、审查、CI 和证据归档。仓库切换后会加载对应 Git 公共目录的记录。

关闭 Studio 时，远程 Worker 可继续执行已派发的 Run。重新打开原仓库，选择原记录并点 **继续任务**，即可恢复跟踪同一票据、候选和 PR。若停在发布或合并确认，重新审查后再确认。已完成的发布或合并会通过回执和 GitHub 状态恢复到后续步骤。

任务失败后点 **重试当前步骤**。尚未派发的任务沿用原 Spec 与目标，Worker 的失败重试在同一票据下开启新 Run，并使用该票据的剩余尝试次数。CI 失败时只重跑同一候选的失败工作流。已保存的失败和重试回执会保留在归档。恢复需要原仓库、原站点及可用的操作员令牌文件。

离屏验收可对已部署的 Docker 站点演练短时断线。提前固定任务和 `test/` 目标分支，并确认同站点没有其他活动工单。调试程序核对 Docker 标签、Studio 的回环 HTTPS 地址和正在检查的 Run，停止该入口超过一个 Lease 后恢复。发布和合并仍分别等待确认。

要验证界面创建，仅提前准备 `test/` 目标分支，并将 `AEP_STUDIO_LIVE_DRAFT` 设为草稿 JSON 的路径。草稿包含 `title`、`spec`、`allowed_paths` 数组和 `acceptance` 对象，`AEP_STUDIO_LIVE_TASK` 使用尚未派发的任务 ID。设置 `AEP_STUDIO_LIVE_RESTART=1` 后，离屏验收会在两次确认前关闭并重新打开 Studio，然后从列表继续原任务。

```sh
export AEP_STUDIO_CONTROL_URL="$url" AEP_STUDIO_CONTROL_CA="$ca"
export AEP_STUDIO_CONTROL_OPERATOR_FILE="$operator_file"
export AEP_STUDIO_LIVE_TASK="$task" AEP_STUDIO_LIVE_TARGET="$target"
export AEP_STUDIO_LIVE_OUTAGE_SITE="$site_name"
cargo run --locked --manifest-path desktop/Cargo.toml --example ui_debug -- \
  --live-https-remote --output-dir desktop/target/live-https-remote
```

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

### 私有 GitHub 仓库

更新 HTTPS 控制面、出口代理、Worker 和 Studio。为新 Worker 绑定 GitHub 仓库，再初始化工作区、安装模型凭证并启动 Worker。多 Worker 站点为对应的 `AEP_PILOT_WORKER_SLOT` 分别绑定仓库。已有 Deploy Key 的 Worker 升级后会识别当前密钥。

```sh
./deploy/pilot/site.sh repo-bind "https://github.com/$repo.git"
./deploy/pilot/site.sh workspace "https://github.com/$repo.git" "$branch" "$git_name" "$git_email"
```

在 Studio 打开私有仓库的本地克隆，进入「HTTPS 试点 → 真实任务」，填写控制面、CA 和操作员令牌路径，再点「连接队列」。GitHub 登录账号需要目标仓库的 Deploy Key 管理权限。

1. 在「Worker Deploy Key」核对仓库、Worker 和指纹。初次配置点「生成密钥」，已有密钥点「准备轮换」。Worker 空闲时会在本机的监督进程私有目录生成新密钥。
2. 点「注册只读并切换」。Studio 将公钥注册为只读 Deploy Key，Worker 访问 GitHub 成功后切换。点「验证只读」可重新核对 GitHub 权限和 Worker 的读取结果。
3. 用新密钥跑一次任务并检查证据。核对待撤销的旧指纹后点「撤销旧密钥」，再点确认。Studio 再查一次生效密钥在 GitHub 上的只读状态，按旧密钥的 GitHub ID 撤销。如果网络中断，重新打开 Studio 可以核对当前阶段并重试。

已有的命令行配置也可以用于初次部署。下面假设 `repo` 是 `OWNER/REPO`，`key` 是操作员私有目录中的新文件路径。

```sh
ssh-keygen -t ed25519 -N '' -f "$key"
gh api --method POST "repos/$repo/keys" -f title="$worker_name" \
  -f key="$(cat "$key.pub")" -F read_only=true --jq '{id,read_only}'
./deploy/pilot/site.sh repo-key "https://github.com/$repo.git" < "$key"
./deploy/pilot/site.sh workspace "https://github.com/$repo.git" "$branch" "$git_name" "$git_email"
```

Worker 监督进程验证 GitHub 主机公钥，经代理连接 `ssh.github.com:443`，并将指定的 Git 引用导入工作区。控制面保存公钥、指纹和操作状态。私钥始终保存在 Worker 的私有挂载或监督进程目录。Studio 用操作员本地 Git 和 `gh` 的登录身份执行任务 Spec 推送、发布和合并。任务归档会记录仓库的可见性，应按私有仓库权限保管。

### 原位升级

先构建本次发布使用的控制面和 Worker 镜像。在部署主机指向原站点名称，使用原操作员令牌和 CA 证书，并选择一个新的收据目录。如果原站点由 Docker 自动分配回环端口，升级会保留当前端口并将其固定下来。收据目录含升级前的控制状态备份，应保存在操作员可访问的私有位置。

```sh
export AEP_PILOT_NAME="$site_name"
export AEP_PILOT_CONTROL_IMAGE="$new_control_image" AEP_PILOT_WORKER_IMAGE="$new_worker_image"
./deploy/pilot/build.sh
./deploy/pilot/site.sh upgrade "$site" "$receipt_dir"
```

命令先检查没有活动工单，再冻结空闲 Worker 并核对 Lease 和本地日志。它保留现有 HTTPS 端口、证书、设备身份、模型凭证、工作区、控制状态、网络和数据卷。新 Nginx 和控制面通过 HTTPS 健康检查后，Worker 会带 `--upload-evidence` 启动。验收会对比旧工单、控制面归档的 SHA-256，以及手工配置 Deploy Key 的指纹。新服务未通过核验时，命令使用原镜像和原数据卷在同一地址回滚，并记录结果。

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
