# PokeOriginal 现在怎样制作并交付一次游戏改动

核对基线为游戏仓库 `dev@5768c9feaecc1fc0786a0e162728da8f0ecf111d`。下表中的路径均相对游戏仓库，可用 `git show <提交>:<路径>` 查看对应版本。表中的工具是可用入口，各工作流的触发范围由对应 YAML 决定。这里记录工具和产物流向，玩法规则继续由游戏仓库维护。

| 工作环节 | 谁提供输入，工具如何处理 | 交付物与检查 |
|---|---|---|
| 需求、规则与排期 | 策划和维护者在 Gitea issue 确认规则；从 `specs/domain/README.md` 进入模块规格。主线用 `docs/modules/story/main-storyline-spec.md` 对照每个 beat，跨模块变更写清失败、恢复和验收条件。 | issue 记录进度与决定，规格记录稳定行为，PR 关联两者。个人记忆可提供检索线索。 |
| 地区素材与剧情 | 丰缘对照绿宝石、关都对照火红、城都对照 Heart & Soul，中文名核对项目指定汉化源。`tools/db/gen_story_tables.py` 把脚本转换成过场步骤、地图回调和坐标触发器；`tools/kanto/`、`tools/johto/` 分别维护地区工具。 | 定点更新对应表和新增迁移。Rust 解读条件与角色状态，下发 CUTSCENE，Godot 执行。序章涉及两套性别镜像，单人单路线走通只覆盖其中一套。 |
| 策划表、种子与存量库 | 内置物种、道具、地图等以 `tables/` 下的 xlsx 为源。`tools/table/gen_gameserver_build.py` 用于从表生成建库种子；已应用的 sqlx 迁移冻结，线上纠正使用新编号迁移。限时活动按运营模块的运行表和发布规则处理。 | `bash test/run_server_guards.sh` 包含从表重建 SQLite、迁移号和数据漂移检查。表值、生成物和旧库升级是三个需要分别核对的结果。 |
| 地图、精灵、界面、文字与音频 | `tools/map/` 处理地图烘焙、分层、传送与动画；`tools/sprite/` 处理精灵图；`tools/text/` 有对白回填和字形工具；`tools/audio/wav_to_ogg.py` 负责音频转换。Godot 从入库的场景、纹理和数据加载。 | `bash test/run_map_guards.sh` 检查地图产物；`.gitea/workflows/client-static-guards.yml` 检查场景 ID、资源路径大小写、UI 资源和音频。新图核 LFS 对象，新脚本核 UID，文本还需在目标界面验缺字、换行与遮挡。 |
| 服务端与联机 | `gameserver/` 是 Rust 游戏后端，协议与剧情调度在 `gameserver/src/wire/`。DAO 守门限制业务层裸 SQL；改 SQL 要在 SQLite 和通过 `tools/qa/pg_test_env.sh` 准备的 PostgreSQL 上跑相应用例。 | 单测看状态和事务；`test/protocol/` 走实际协议。交易、奖励等涉及两人或重试时，应断言双方最终状态、重复请求效果。客户端显示成功不能证明数据库提交成功。 |
| 客户端与体验 | Godot 4.7.2 加载 `client/project.godot`。`test/mcp/catalog.json` 按业务模块索引运行中客户端的用例，`test/mcp/mcp_runtime_scene_smoke.py` 检查脚本和 UI 场景实例化，使用时显式指定匹配的 Godot。 | 登录场景可启动、场景可实例化、玩家在世界中走完操作分别对应不同覆盖面。根据需求追加键鼠、触屏、对话焦点、双端同步或真机帧时间证据。安卓跨图性能已有 `test/perf/android_seam_profile.py`。 |
| 合入与发版 | 功能分支通常开 PR 到 `dev`。`.gitea/workflows/server-guards.yml` 的 PR 分支过滤针对 main/release，Cargo 作业有手动触发条件；进入 dev 前要显式安排所需检查。地图和客户端静态检查另有各自工作流。 | `.gitea/workflows/client-release.yml` 有桌面整包、桌面补丁、安卓整包、安卓补丁四种手动入口。桌面和安卓各有自己的基线。服务端由 `tools/deploy/deploy_gameserver.sh` 构建、预演迁移、切换和核验。 |
| 上线反馈 | 客户端经独立 collector 上报错误、性能与卡顿，设计见 `docs/technical/client-telemetry-service.md`。维护者在对应版本定位问题并回到 issue。 | 用发布版本、平台、复现场景关联证据。提供聚合指标或脱敏摘录，原始玩家日志留在游戏侧。 |

## 哪些工作值得交给 Game AEP

1. **选检查和准备环境。** 同一处剧情修改可能同时涉及地区源码、表、迁移和客户端演出。把这些依赖从个人经验变成可审查的检查选择，并在运行前列出所缺资源。
2. **让二进制改动可审。** Git 文件列表只能说明哪张表或哪张图变过。表格要给出稳定业务键的行列差异，地图/美术要给出受影响地图与同场景前后对照，LFS 指针还需确认真实对象可取得。
3. **明确覆盖到哪一层。** 进程退出码、模拟动作、真实触屏和人工确认分别标记。把缺席的设备、没有执行的断言和用例依赖写入结果。
4. **把版本串起来。** 让策划能从 issue 找到规则与效果，让程序能从失败验收找到命令、夹具和候选提交，让发布人员能核对自己将要发布的基线和构建产物。

平台如何选择检查、保存这些证据并逐步接入，见[设计与验收](design.md)。
