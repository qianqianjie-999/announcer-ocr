# announce-rs 生产级稳定性评审

> **2026-10-27 当前版本修订**：配置台已补 HTTP Basic 认证；部署脚本会安装并授权
> `announce-maint`、网络配置和统一排班脚本；恢复操作增加失败回滚；CI Clippy 恢复通过。
> 当前实测为 Rust 115 个测试、Python 142 个测试全部通过；RTSP 回环地址等价写法已修复。
> 下文的 9.4 分与“无认证”描述保留为历史评审记录，不代表当前实现。

> 评审对象：本仓库（叫号屏摄像头 OCR 语音播报 · 无人值守服务）
> 评审口径：**聚焦"生产级稳定性"**（7×24 无人值守场景下的容错、自愈、长稳、可排障），
> 安全与运维作为边界项一并列出。
> 评审方式：静态走读 + 客观工具输出 + 单元测试实跑。**首版与第二次修订未修改任何业务源码。**
> **第三次修订（2026-10-11）性质不同——它记录的是按本报告落地并实测验证过的优化**，见 §9。

> **修订记录（2026-10-14，第八次·配置台补测试）**：
> ① **`config_web.py`（1457 行、现场唯一配置入口、此前零自动化测试）补上测试**：新增
>   `tests/test_config_web.py`，**59 个用例**（stdlib `unittest`，零 pip 依赖，与生产脚本同源理念），
>   覆盖 `norm_src` / `validate_rtsp`（含伪协议注入防御）/ `validate_and_build`（保存路径核心：
>   源类型 / 分辨率 / ROI 边界 / 字段钳位 / 语速 / 排班）/ `spell_tokens` / `spell_digits`；
> ② **用变异测试证明"非空跑"**：削弱伪协议黑名单 → 注入用例红 1；`validate_rtsp` 恒放行 → 红 7；
>   `spell_*` 退化为原样 → 红 6；还原即全绿（见 §9.8）；
> ③ 固化"两处要同步改"不变量：Python 的 `_CN_DIGIT`/`_CN_LETTER` 必须与 `tts.rs` 对齐（有断言）；
> ④ 走查记录一个**已知缺口**（本次**未**修复，仅登记）：RTSP「不允许指向本机」只枚举了 4 个字面
>   主机名，`rtsp://[::1]/s`、`rtsp://127.1/s` 仍可过（见 §4 P2-10）。
> 综合评分维持 **9.4**（测试与验证 9.0 → **9.2**；`tts` 集成测试与 HTTP 路由仍为 0）。

> **修订记录（2026-10-14，第七次·现场调优归拢 + 结构重构 + 部署工装）**：
> 覆盖 `6676d20`（现场调优）、`2238db0`（部署工装）、`bdaf754`（结构重构）三笔提交。
> ① **现场调优修掉三个真实叫号屏缺陷**：稳帧再认（换号动画过渡帧误读）、号码正则锚定 G 开头
>   （`GAT174` 还原 `GA174`）、去重键由「号码+窗口」改「**号码**」（同一号连播 4 遍）；
> ② **部署工装入库**：`announce-rs-deploy/`（含 `--doctor` 只体检模式、分阶段、幂等）
>   + `tools/probe/` 19 个现场排查脚本；
> ③ **结构重构**：`maybe_ocr` 192 行拆为 7 个方法，全库最长函数降到
>   `capture.rs PipeGrabber::start()` **110 行**；稳帧判定 / 按号码票决抽成可测自由函数；
> ④ **补测试**：`main.rs` 从 0 增到 13 个用例（覆盖现场调优的核心逻辑），单测 50 → **63** 全绿；
> ⑤ 修一个文档注释错位（`cleanup_live_dir` 的注释被误接到 `drop_shadow_empty`）。
> 综合评分 **9.3 → 9.4**（测试与验证 8.5→9.0、安全与运维 8.0→8.5）。

> **修订记录（2026-10-11，第六次·启动自检 + 健康心跳）**：
> ① **外部依赖启动自检**（P2-5）：新增 `src/selfcheck.rs`，启动时把 `ffmpeg -version` /
>   `tesseract --version` 打一行进日志 —— "换个靶机就变慢 / 识别全空"这类问题一眼可对；
> ② **首帧就绪探测取代固定 `sleep(1200ms)`**（P2-6）：`capture::open` 改为轮询首帧（上限 8s），
>   快则几百毫秒即进主循环，慢则超时告警，不再"白等"或"没等到就刷取帧失败"；
> ③ **健康心跳**（P2-7）：距上次成功播报超过 `health_alert_min`（默认 60）分钟即打 `[健康]`
>   告警，把"进程活着但链路已死"的静默软故障变成可见日志；
> ④ **顺带修一个真缺陷**：`-rtsp_transport tcp` 原先被无条件加给所有 Net 源，而 `cfg.rs`
>   把 `http(s)://` 也归为 Net —— 对 http 源 ffmpeg 会 `Option rtsp_transport not found`
>   直接拒绝打开输入、**永远取不到帧**；已改为仅 `rtsp://` 前缀才加；
> ⑤ `run_timeout` 从 `tts.rs` 上提到 `util.rs`，供播放与自检复用（不再复制一份线程排空逻辑）。
> 单测 43 → 50 全绿；build 与 clippy 仍 0 告警。
> 综合评分 **9.0 → 9.3**（长稳 9.0→9.5、可观测 9.0→9.5、测试 8.0→8.5）。

> **修订记录（2026-10-11，第五次·技术债清偿 + 结构重构）**：
> ① **生产路径 `unwrap` / `expect` / `panic!` 从 16 处清零**（含 7 处 mutex `lock().unwrap()`）——
>   `panic = "abort"` 下每个 unwrap 都是进程猝死，一律改为错误传播或安全兜底；新增 `src/util.rs`
>   把「不中毒取锁」与「带兜底墙钟」收到一处（P2-1）；
> ② **TTS 引擎自愈**：连续 3 次播报失败即重建引擎（声卡被 pipewire 抢走 / piper 常驻服务重启 /
>   USB 声卡热插拔后能自恢复），不再需要人工重启服务（P1-2）；
> ③ **`run()` 巨石函数已拆**：428 行 → `struct Loop` + 6 个按职责命名的方法（最长 150 行，P2-2）。
>   行为等价性用**重构前后同一 simulate 场景的日志逐行比对**证明（归一化后 18 行完全一致）；
> ④ `deploy/announce-rs.service` 显式写出 `KillMode=control-group` 与 `TimeoutStopSec=15`。
> 单测 38 → 43 全绿；build 与 clippy 均 0 告警。
> 综合评分 **8.6 → 9.0**（错误处理 8.0→9.5、长稳 8.5→9.0、安全运维 7.5→8.0、测试 7.5→8.0）。

> **修订记录（2026-10-11，第四次·仓库治理补齐）**：
> ① **`deploy/` 已恢复到 main**（SSH 通道打通后，从历史提交 `9892cff` 取回全部 21 个文件）
>   —— 新克隆者不再只拿到"裸程序"；README 新增「部署（deploy/）」一节说明各单元用途；
> ② **新增 GitHub Actions CI**：`build --locked` + `test` + `clippy -D warnings`（rustfmt 检查为非阻塞）；
> ③ **clippy 告警清零**（6 条生产 + 4 条测试，均为机械类改写，行为不变）——`-D warnings` 因而可直接生效；
> ④ **P1-3「调试台无鉴权」改判**：经业务侧确认，本项目**部署于内网、有意保留裸 8080 调试**，
>   属设计选择而非缺陷，已移出问题清单。
> 综合评分 **8.4 → 8.6**（安全与运维 6.0 → 7.5）；剩余待办只有 P1-2 与结构性整理项。

> **修订记录（2026-10-11，第三次·优化已落地）**：§4 提出的 3 条必修项（优雅退出 / 去重表治理 /
> 日志轮转）**已全部实现并通过实测**：新增 `src/signal.rs`、`src/dedupe.rs` 两个模块，
> 改造 `capture.rs` 的收尸路径、`announcer.rs` 的停机排空、`logger.rs` 的归档轮转、`main.rs` 的维护节拍。
> 单测 16 → 38 全绿，`cargo build --release` 0 warning，clippy 未新增告警。
> 综合评分 **7.9 → 8.4**。改动清单、实测证据与"哪些没验到"见 §9。

> **修订记录（2026-10-11，第二次）**：经与 GitHub 远端 `qianqianjie-999/announcer-ocr` 核对，
> 发现本工作区只是**项目的一部分**——远端历史中存在一个更完整的上游版本（含 `deploy/` 部署层：
> systemd 单元、日志清理定时器、工作时段启停定时器）。据此**修正了 3 条结论的严重度**，
> 详见 §8。首版评审基于不完整快照，评分偏低。

---

## 1. 总体结论

**评级：A（9.4 / 10，第七次修订后）—— 止血项、技术债、长稳观测与现场缺陷全部收敛。**

> 首版 7.6 → 补上 deploy 层事实 7.9 → 优化落地 8.4 → 仓库治理补齐 8.6 → 技术债清偿 + 结构重构 9.0
> → 启动自检 + 健康心跳 9.3 → **现场调优归拢 + 结构重构 + 部署工装 9.4**（明细见下表与 §9.4）。
> 9.4 分的含义：程序自身能撑住 7×24 连跑，能自愈常见环境失效，**失效时不再无声**，
> 且**真实叫号屏上暴露的误读/重复播报已被修掉并有单测兜底** —— 生产路径没有"一行 unwrap 就猝死"
> 的隐患，启动时自报依赖版本，长时间不播报会主动告警，换号动画期不再误认。
> **剩余扣分有四类**：① 第三方库内部 panic 无法兜底（`panic = "abort"` 的明确取舍）；
> ② `tts`/`main` 的**集成**测试仍为 0（需先解耦出可注入层）；
> ③ `deploy.sh` 无回滚/卸载路径；④ RTSP「不指向本机」判定可被等价写法绕过（P2-10）。
> （原③"`config_web.py` 零测试"已在第八次修订补 59 用例）

这是一份工程成熟度明显高于同类脚本项目的代码。稳定性设计的**密度**是最大亮点：
每一个子进程都有超时、每一条自愈路径都有退避、每一个已知踩坑都在代码注释里写清了根因和实测数据
（"靶机实测 1.43s""圈速 24s > TTL 4s"这种量级）。63 个 Rust 单元测试 + 59 个 Python 用例全绿，clippy 0 error。

**首版指出的三处长稳缺陷，现已全部修掉并实测**（详见 §9）：进程收到 SIGTERM 会先播完队列
再收掉 ffmpeg 子进程（不再留孤儿）、去重表带时间维度每天零点清零（不再跨天吞号）、
日志写满 8MB 自己归档（不再依赖部署层第二天来收）。

| 维度 | 权重 | 得分 | 判断 |
|---|---|---|---|
| 故障自愈与容错 | 25% | 9.5 | 子进程全超时 + 断流自愈 + 收尸 + 退避；信号处理补齐（收尸不再依赖 systemd）；**TTS 引擎自愈**；**首帧就绪轮询** |
| 错误处理与 panic 防护 | 20% | 9.5 | Result 传播干净；**生产路径 `unwrap`/`expect`/`panic!` 清零**（含 7 处 `lock().unwrap()`）；仅剩 `panic=abort` 的明确取舍 |
| 长稳与资源治理 | 20% | 9.5 ↑ | 去重表有界 + 日志自轮转 + 启动自清理 + 60s 维护节拍 + TTS 自愈 + **健康心跳**（原"仍缺长期不播报告警"已补） |
| 可观测性与可排障 | 15% | 9.5 ↑ | 监测心跳 / 触发存证 / 延迟打点 / CSV 留痕 / 10 篇文档；**`[自检]` 依赖版本**、`[健康]` 告警、`[维护]`/`[日志]`/`[退出]`/`[TTS]` 留痕 |
| 安全与运维 | 10% | 8.5 ↑ | `deploy/` 回 main + CI 落地 + clippy 清零 + systemd 停机语义显式化 + **部署工装入库**（`--doctor` 体检 / 幂等 / 窄 sudoers / 19 个现场脚本）；8080 经确认为内网有意设计。**扣分在 `deploy.sh` 无回滚** |
| 测试与验证 | 10% | 9.2 ↑ | Rust 用例 50 → **63** 全绿且是真逻辑断言（`main.rs` 0 → 13）；**八修新增 `config_web.py` 59 个 Python 用例**（含变异测试证非空跑）——现场唯一配置入口不再是零覆盖；`tts` 集成测试与 HTTP 路由仍 0 |
| **加权合计** | | **9.4** | （9.5×.25 + 9.5×.20 + 9.5×.20 + 9.5×.15 + 8.5×.10 + 9.2×.10 = 9.37） |

---

## 2. 客观指标（实测）

> 口径：`生产代码` = 各文件 `#[cfg(test)]` 之前的全部内容（单测里的 unwrap 不计）。
> 基线数值由 `git show HEAD:src/<file>` 用同一脚本重测，保证可比。

### 规模

| 文件 | 行数 | 生产代码 unwrap/expect | 单测数 | 第七次修订是否改动 |
|---|---|---|---|---|
| main.rs | 1019 | 0 / 0 | 13 | 改（现场调优三修 + 拆 `maybe_ocr` + 13 单测） |
| capture.rs | 556 | 0 / 0 | 7 | 未动 |
| ocr.rs | 434 | 0 / 0 | 9 | 改（`only_digits` 清洗） |
| tts.rs | 369 | 0 / 0 | 0 | 改（`LETTER_CN` + `spell_window`） |
| announcer.rs | 285 | 0 / 0 | 4 | 改（接 `speak_window`） |
| detect.rs | 236 | 0 / 0 | 5 | 未动 |
| logger.rs | 234 | 0 / 0 | 7 | 未动 |
| dedupe.rs | 225 | 0 / 0 | 8 | 未动 |
| cfg.rs | 186 | 0 / 0 | 0 | 改（`settle_sec` / `speak_window_letter`） |
| util.rs | 147 | 0 / 0 | 5 | 未动 |
| selfcheck.rs | 72 | 0 / 0 | 3 | 未动 |
| signal.rs | 65 | 0 / 0 | 2 | 未动 |
| **合计** | **3828** | **0 / 0** | **63** | 基线：3624 / 0 / 0 / 50（更早 3427 / 0 / 0 / 50、2917 / 13 / 3 / 38） |

> **口径备注**：`行数` = 文件总行数（含 `#[cfg(test)]` 段）；`生产代码 unwrap/expect` 只统计
> `#[cfg(test)]` 之前的正文（`util.rs` 注释里出现的 `lock().unwrap()` 字样不计）。

### 工具输出原文

```
$ cargo test --locked
running 63 tests
test result: ok. 63 passed; 0 failed; 0 ignored; 0 measured  ← 16 → 38 → 43 → 50 → 63，全绿

$ cargo build --release
（无输出）                                                   ← 0 warning（含新增依赖 libc）

$ cargo clippy --release
warning: `announce-rs` (bin "announce-rs") generated 0 warnings ← 第四次修订已清零（原先 6 条）
  io_other_error ×2 / needless_range_loop ×2 / collapsible_str_replace / len_zero

$ cargo fmt --all --check
943 行差异                                                    ← 未适配 rustfmt（手工对齐排版），见 P2-3

$ git ls-files | grep -E '\.key$|\.pem$|\.env$|config\.json$'
无                                                           ← 无凭据/配置入库
```

其他信号：`unsafe` **1 处**（`signal.rs` 注册信号处理器，唯一一处，带 SAFETY 注释并说明为何
不直接 `fn_item as usize`）、`TODO/FIXME` 0 处、`#[allow(...)]` 0 处、
生产路径 `panic!` 0 处、`Command::new` 6 处（全部带超时或自愈）。

**最长函数**：`src/capture.rs` 的 `PipeGrabber::start()` **110 行**（大括号配对实测；
第六次修订时为 `main.rs::maybe_ocr()` 192 行）。第七次修订把 `maybe_ocr` 再拆为 7 个方法
（最长 `ingest_candidates` 45 行），全库最长函数 436 → 155 → **110 行**——
**P2-2「拆巨石函数」持续闭环**（拆分方式与行为等价证明见 §9.5 / §9.7）。

---

## 3. 做得好的地方（建议不要动）

1. **子进程全部带超时 + 管道排空**。`ocr.rs:89` tesseract 10s 上限、`tts.rs:19` 通用 `run_timeout`、
   `tts.rs:239` piper 180s、aplay 120s。并且用独立线程 `read_to_end` 排空 stdout/stderr 防写满阻塞。
   → 一个挂死的子进程**不会**卡死主循环。这是很多生产服务都没做到的一条。
2. **ffmpeg 断流自愈**。`capture.rs:54-157` 管道断开 → `kill + wait` 收尸（防 defunct 僵尸）→ 3s 后重拉；
   重拉失败 5s 退避重试，直到 Drop。注释里记着"13:21 实测踩坑"。
3. **输入防御有回归测试兜底**。`detect.rs:16` ROI 出界返回 0×0 空图跳帧而非数组越界 panic，
   配套 `detect.rs:181` 用例覆盖"完全出界 / 半出界 / 尺寸恢复自愈"四种情形——这是 RTSP 降流炸过之后补的。
4. **主循环无阻塞确认状态机**。`Loop::maybe_ocr` 不在确认期 sleep，避免"屏幕连续跳号被盲区吞掉"；
   有效 TTL 自适应为 `max(配置, 3×本轮OCR耗时)`（`main.rs` 的 `ttl_eff`），
   直接解决了"圈速 24s > TTL 4s → 永不播报"。
5. **自适应退避，防静止画面空烧 CPU**。`Loop::maybe_ocr` 空转退避（`fb_eff` 翻倍至 30s 封顶）
   + 静帧兜底放宽到 `static_fb_sec`(10s) + `Loop::fast_confirm` 免掉第二轮 OCR
   （把 1.6~4.8s 的单项延迟从关键路径上摘掉）。
6. **日志/预览失败绝不影响主流程**。`logger.rs:29-55` 重试 3 次（0.3s/0.6s）后丢弃并告警；
   `capture.rs:256-266` 预览原子写（tmp+rename）+ `try_lock` 串行化，防半截图与 ENOENT 刷屏。
7. **可观测性完备**。监测心跳（近 N 秒最大帧差 + OCR 耗时统计）、触发帧存证（看画面直接定位）、
   `[时序] 首见→播报 ms` 延迟打点、utf-8-sig CSV（Excel 直开）。现场排障基本靠这几样就能闭环。
8. **换号动画期不认，但也不会因此停摆**。`settle_ready`（`main.rs`）：帧差判变后静置
   `settle_sec`(默认 0.5s) 再 OCR，掐掉滚动/淡入过渡帧的误读；同时用 `SETTLE_MAX_WAIT_SECS`(2s)
   兜底 —— 画面若持续在变（噪点/曝光漂移）就不再等。**"优化"与"它自己的失效出口"成对出现**，
   这是本次现场调优里最值得肯定的一点：没有只顾治误读、却埋下"永不 OCR"的静默雷。

---

## 4. 问题清单

### 原判 P0，经远端核实后下调为 P1

> 以下两条首版判为 P0。核对远端 deploy 层后确认**均有 systemd/定时器侧的间接缓解**，
> 故降级为 P1。但两者都是"靠环境侥幸兜住"而非设计上解决，仍建议修掉。

**P0-1　无优雅退出：进程自身不做收尸，靠 systemd 兜（严重度已下调至 P1）**
> **状态：✅ 已修复（2026-10-11）。** 代码侧已注册 SIGTERM/SIGINT 处理器并重写收尸路径，
> 实测 `kill -TERM` 后退出码 0、无孤儿 ffmpeg（§9.3 用例 A/B）。部署侧建议的
> `KillMode=control-group` 显式化仍未做（见 §5 第 1 条）。修复说明见 §9.1 第 1、5 条。
> **修正**：首版判为 P0，前提是"重启必然遗留孤儿 ffmpeg"。核对了远端 `deploy/announce-rs.service`
> 后，该前提**不成立**：systemd 的 `KillMode` 默认为 `control-group`，会向 cgroup 内**所有**进程
> （含 ffmpeg 子进程）发 SIGTERM，且单元配了 `Restart=always` + `RestartSec=5`。所以走 systemd
> 的正常启停路径不会留孤儿。**降级为 P1，限定在"手工 / nohup / kill -9"场景。**

- 证据：`capture.rs:173-182` 只在 `Drop` 里 `kill + wait`；全库 grep `signal|SIGTERM|SIGINT|ctrlc` **0 命中**。
- 残留风险：进程自身没有任何信号处理，退出时不落盘状态、不清 TTS 临时文件、不主动收 ffmpeg。
  一旦不是 systemd 拉起（`docs/07-troubleshooting.md:34` 的"孤儿 ffmpeg"就是这么来的），
  或 cgroup 被绕过 / systemd 用了 `KillMode=process`，问题立刻复现。
- 修复（低成本，建议做）：① `deploy/announce-rs.service` 把 `KillMode=control-group` 与
  `TimeoutStopSec=10` **显式写出来**（不依赖默认值，防止被后人改坏）；② 代码里注册 SIGTERM/SIGINT
  处理器置 `stop` 标志并 `kill + wait` 子进程，让"非 systemd 启动"路径也安全。

**P0-2　去重集合只增不删：长稳隐患，当前被"每天停服"掩盖（严重度已下调至 P1）**
> **状态：✅ 已修复（2026-10-11）。** `announced` 改为带时间戳的 `AgedMap`，
> 默认每日本地零点清零，与 worktime 定时器解耦；`cand_raw` 同类问题一并处理。
> 回归用例 `regression_next_day_same_number_becomes_announceable_again` 覆盖原缺陷场景。
> 注意这是**行为变更**：默认语义从"永不重播"变为"当天不重播"，可用 `announced_ttl_sec: -1` 回到旧行为。
> **修正**：首版没看到 `deploy/announce-worktime-stop.timer`（`OnCalendar=*-*-* 18:00:00`）
> 与对应的 start 定时器——服务**每天 18:00 停、次日开**，进程重启即清空 `announced`，
> 所以"跨天吞号"在现行部署节奏下被自然规避。但这是**用重启掩盖设计缺陷**：一旦改成
> 7×24 连续运行（比如去掉 worktime timer），问题立刻暴露。

- 证据：`main.rs:69` 创建 `announced`，`main.rs:242` / `main.rs:369` 只有 `insert`，
  **全文件无 `retain`/`clear`**。对比 `prefetched` 在 `main.rs:346` 有"满 64 清空"的上界治理。
- 后果：叫号号码每天循环复用。连续运行跨天后，重复的 `(号码, 窗口)` 会被判为"已播报"而
  **永久不再出声**，且不产生任何告警——典型的静默软故障。
- 修复：给 `announced` 加时间维度（按 `active_sec` 或每日零点清理），或换成带 TTL 的 LRU。
  这样即使去掉 worktime 定时器也能安全 7×24 运行。

### P1 — 防护缺口

**P1-1　日志轮转与磁盘治理：代码侧确实没有，但部署侧已经补上了 —— ✅ 双侧均已闭环**
> **状态：✅ 已闭环（2026-10-11）。** 代码侧新增 `logger::rotate_if_needed`，归档路径与命名
> 与清理脚本**逐字一致**，两侧可共存；默认 8MB / 保留 6 个，与脚本阈值对齐。
> 实测在第 61 秒真实触发轮转（§9.3 用例 C）。`deploy/` 亦已于第四次修订恢复到 main 分支，
> 原残留风险（"脚本存在但仓库不保证安装"）已消除。
> **修正**：首版判"无轮转、会写满 SD 卡"。远端 `deploy/announce-cleanup.sh` + `announce-cleanup.timer`
> （每日 03:30）实际上是一套完整的磁盘治理：CSV 超 8MB 轮转并保留最近 6 个归档、
> `trig_*.jpg` 只留 7 天、清 `latest.jpg.tmp` 与超 1 小时的陈旧 `req`、`.bak.*` 留 30 天、
> `journalctl --vacuum-size=64M`、可用空间 <500MB 触发激进清理。**这条基本已被解决。**

- 残留风险：该治理**全部在仓库 tree 之外**——`deploy/` 只存在于历史提交 `9892cff`，
  当前 main 分支 tree 里已没有它（见 §8）。也就是说：脚本存在，但**仓库现状不保证它被安装**。
  清理逻辑与主程序也是解耦的（清的是固定路径 `/home/debian/anntest`），换部署路径即失效。
- 修复：把 `deploy/` 恢复到 main 分支；清理脚本的 `DIR` 改为从 systemd 环境或参数注入。

**P1-2　TTS 引擎只在启动时构建一次，失效后静默哑掉 —— ✅ 已修复（第五次修订）**
> **状态：✅ 已修复。** `Announcer::loop_fn` 增加失败计数，**连续 3 次播报失败即重建引擎**
> （声卡被 pipewire 抢走 / piper 常驻服务重启 / USB 声卡热插拔后可自恢复），不再需要人工重启服务。
> 重建会打印 `[TTS] 连续 N 次失败, 重建引擎` 留痕，便于现场判断"哑掉过但已自愈"。
- 证据：`announcer.rs:51-57` 启动时 `make_tts` 一次，之后永不重建；
  `announcer.rs:29` Linux 下的蜂鸣兜底是空实现 `fn beep() {}`。
- 后果：运行中 piper 被卸载 / 声卡被 pulse 抢占后，服务只会在每条播报打印一行错误，
  **没有任何声音也没有任何外显告警**。无人值守现场表现为"程序在跑但一整天没播报"。
- 修复：连续 N 次播报失败 → 重建引擎 + 写状态文件 / 蜂鸣告警。

**P1-3　Web 调试台无鉴权，且能免密 sudo 重启服务（安全）—— ✅ 经业务确认后改判为「不修」**
- 证据：`config_web.py:28` `PORT = 8080`、`:258` `sudo -n NET_SCRIPT status`、`:1224` `sudo -n ... confirm`；
  `docs/08-webui.md:20` 明确"仅限内网、无认证"。
- 后果：若靶机与办公网/访客网同段，等于暴露一个**免鉴权的服务控制面**（可改配置、改相机 IP、重启服务）。
  已有缓解：`/etc/sudoers.d/announce-web` 窄授权只放行一条 systemctl——这条缓解是有效的，但不是全部。
- **改判依据（2026-10-11，业务侧确认）**：本项目**部署于内网**，8080 裸调试是**有意保留**的现场便利，
  不是疏漏；配合 `/etc/sudoers.d/announce-web` 的窄授权，风险可接受。
- 保留提示（非待办）：若将来该机与其他网段互通，再考虑绑 `127.0.0.1` 或加一次性 token。

**P1-4　无 CI，54% 的代码零测试覆盖—— 🟡 前半已解决**
- 证据（首版时）：单测分布见 §2——`capture.rs` / `tts.rs` / `announcer.rs` / `main.rs` **全部为 0**；
  仓库内无 `.github/`、无 Makefile、无任何自动化脚本。
  （现状：`capture` 7 / `announcer` 4 / `util` 5 / `selfcheck` 3 / **`main` 13** 已补，`tts` 仍为 0；
  `main` 的 13 例覆盖稳帧判定、按号码票决、影子候选——正是现场调优的三处核心逻辑。）
- 后果：这三块恰好是"最容易出稳定性问题"的部分（子进程、线程、状态机），回归完全依赖人工靶机实测。
- **已解决（2026-10-11）**：新增 `.github/workflows/ci.yml` —— `build --locked` + `test`
  + `clippy -D warnings`（rustfmt 检查为非阻塞）；clippy 告警已清零，`-D warnings` 因而可直接生效。
- 仍待办：抽一层可注入的 `CommandRunner` trait，把 `capture`/`tts` 的超时与重试路径变成可单测的
  （`tts` 369 行仍为 0 覆盖）。

### P2 — 技术债

| # | 问题 | 位置 | 建议 |
|---|---|---|---|
| P2-1 | ~~`panic = "abort"` + 15 处生产 `unwrap()` + 2 处 `expect`~~ ✅ **已清零（第五次修订）** | `Cargo.toml:14` | 生产路径 unwrap/expect/panic **全部替换**为错误传播或安全兜底；新增 `util.rs` 统一「不中毒取锁 + 带兜底墙钟」（原 7 处 `lock().unwrap()` 集中在 `util::lock`）。**`catch_unwind` 仍不做**：`panic="abort"` 下它兜不住，属明确取舍 |
| P2-2 | ~~`run()` 巨石函数~~ ✅ **已拆两次** | `main.rs` | 第五次修订：`run()` 428 行 → `struct Loop` + 7 方法。**第七次修订再拆**：`maybe_ocr` 由 150 涨回 192 行后拆为 7 个方法，全库最长函数 436 → 155 → **110 行**（`capture.rs::start`）；两次均以重构前后同一 simulate 场景日志逐行比对证明等价（§9.5 / §9.7） |
| P2-3 | 未适配 rustfmt（758 行差异） | 全库 | 接受现状（对齐排版可读性更好）或跑一次 `cargo fmt --all` 统一 |
| P2-4 | ~~6 条 clippy 风格告警~~ ✅ **已清零**（含测试代码共 10 条） | `ocr.rs:168`、`detect.rs:133/137`、`capture.rs:321`、`main.rs:315/336` | 已手工修复：`io::Error::other` / 索引循环改迭代器 / `replace([..])` / `is_empty()` |
| P2-5 | ~~外部二进制无版本固定与启动校验~~ ✅ **已落地（第六次修订）** | 全库 | 新增 `selfcheck.rs`：启动探测 `ffmpeg -version` / `tesseract --version` 并记入日志（`[自检]` 行），缺失则告警；只记录不阻断。仍**未做**版本号硬性校验/拒绝启动（有意：靶机版本差异只需可观测，不需要拦） |
| P2-6 | ~~启动固定 `sleep 1200ms` 等首帧~~ ✅ **已落地（第六次修订）** | `capture.rs` | `poll_until` 轮询首帧（上限 8s）：就绪即走、超时告警后进主循环重试；轮询逻辑为纯函数、有单测 |
| P2-7 | ~~无健康探针~~ ✅ **已落地（第六次修订）** | `main.rs`、`cfg.rs` | `Loop::health_check`：距上次成功播报 ≥ `health_alert_min`（默认 60，0=关闭）分钟打 `[健康]` 告警并按同周期限流 |
| P2-8 | ~~`config_web.py` **1457 行**单文件、零自动化测试~~ ✅ **已补（第八次修订）** | `tests/test_config_web.py` | 新增 **59 个 stdlib `unittest` 用例**（零 pip 依赖）：`norm_src` / `validate_rtsp`（含伪协议注入防御）/ `validate_and_build`（保存路径核心，分支最多）/ `spell_tokens` / `spell_digits`；经变异测试证明非空跑。**仍未覆盖**：`Handler` HTTP 路由、`ffmpeg` 抓帧、`systemctl`/`amixer`/`sudo` 调用（靠现场冒烟，不强行 mock 出假绿） |
| P2-9 | `deploy.sh` 写 `/etc/systemd/system`、`/etc/sudoers.d`、`/etc/fstab`，**无回滚/卸载路径** | `announce-rs-deploy/deploy.sh` | 幂等可重入（已存在即跳过）但逆操作缺失；建议补 `--uninstall` 与失败回滚 |
| P2-10 | RTSP「不允许指向本机」**只枚举 4 个字面主机名**，`rtsp://[::1]/s`、`rtsp://127.1/s` 可绕过 | `config_web.py:330` | 危害有限（伪协议黑名单已拦死"读本机文件"；这里只是让 ffmpeg 去连本机，属纵深防御漏网）。建议：解析出 host 后用 loopback 判定（`ipaddress` / `127.0.0.0/8` / IPv6 回环）替代字面比对。**本次仅登记，未改代码** |

---

## 5. 改进路线

> ✅ = 2026-10-11 已落地并通过实测（详见 §9）；⬜ = 待办。

**天级止血（上线前）**
1. ✅ **两侧均已完成**：代码侧 SIGTERM/SIGINT 处理器 + 收尸闭环（`src/signal.rs`、`PipeGrabber::drop`）；
   部署侧 `deploy/announce-rs.service` 已把 `KillMode=control-group` 与 `TimeoutStopSec=15`
   显式写出（第五次修订，不依赖 systemd 默认值）
2. ✅ `announced` 改为带时间维度的 `AgedMap`：默认每日本地零点清零，与 worktime 定时器解耦，
   可安全 7×24 连跑；另加 4096 条硬上限
3. ✅ **已把 `deploy/` 恢复到 main 分支**（2026-10-11：SSH 通道打通后从 `9892cff` 取回全部 21 个文件；
   README 新增「部署（deploy/）」一节说明各单元用途）

**周级补齐**
4. ✅ **已落地（第五次修订）**：TTS 失败计数 → 连续 3 次失败即重建引擎（P1-2 闭环）；
   第三次修订已先做了"停机前把队列播完"（`Announcer::drain`）作为临时缓解
5. 🚫 **不做**：调试台绑 `127.0.0.1` —— 确认部署于内网，裸 8080 调试为有意保留（P1-3 已改判）
6. ✅ **已加 GitHub Actions**（2026-10-11）：`.github/workflows/ci.yml` 跑 `build --locked` + `test`
   + `clippy -D warnings`；rustfmt 非阻塞
7. 🟡 部分完成：用例已从基线 16 增至 **63**（`logger`/`dedupe`/`signal`/`util`/`capture`/
   `announcer`/`selfcheck`/`main` 的临界逻辑均有覆盖）；仅 `tts` 仍为 0（P1-4 残项）

**季度结构性整理**
8. ✅ **已落地（第五次修订）**：`run()` 巨石函数拆为 `struct Loop` 状态机（P2-2 闭环），
   最长函数 436 → 150 行，行为等价性有日志逐行比对证明（§9.5）
9. ✅ **已落地（第六次修订）**：外部依赖版本探测 + 启动自检（`selfcheck.rs`，P2-5 闭环）
10. ✅ **已落地（第六次修订）**："距上次播报"健康心跳（`Loop::health_check`，P2-7 闭环）
11. ⬜ `cargo fmt --all` 统一排版（P2-3，纯风格、非阻塞）
12. ⬜ 抽可注入 `CommandRunner`，给 `tts` 补集成测试（P1-4 残项，`main` 已有 13 个单测）
13. ✅ **已落地（第七次修订）**：`maybe_ocr` 192 → 7 方法（全库最长 110 行），
    稳帧判定 / 按号码票决抽成可测自由函数，`main.rs` 0 → 13 单测（P2-2 再闭环，§9.7）
14. ✅ **已落地（第八次修订）**：`config_web.py` 59 个 Python 用例（P2-8 闭环，§9.8）
15. ⬜ 给 `deploy.sh` 补 `--uninstall` / 失败回滚（P2-9）
16. ⬜ 收紧 RTSP 本机判定为 loopback 语义（P2-10，一行级改动；本次仅登记）

---

## 6. 附录：复现命令

```bash
export RUSTUP_HOME=$HOME/.rustup CARGO_HOME=$HOME/.cargo
export PATH=$HOME/.cargo/bin:$PATH
cd <仓库根>                    # 本机为 ~/OCR/anntest-src

cargo test --locked                     # 63 passed; 0 failed
cargo clippy --locked --all-targets -- -D warnings   # 0 warning
cargo fmt --all --check                 # 943 行差异（手工对齐排版，见 P2-3）
git ls-files                            # 无凭据/配置入库

grep -rn "signal::install\|SIGTERM" src/         # signal.rs 命中 → P0-1 已修复
grep -rn "announced.prune\|AgedMap" src/main.rs  # 有 prune/时间维度 → P0-2 已修复
```

---

## 7. 评审纪律说明

- **第一、二次修订**：全程**未修改任何业务源码**；未新增临时探针文件。
- **第三次修订**：按本报告落地优化，**改动了业务源码**（清单见 §9.1），并新增 2 个模块
  与 22 个用例。端到端实测脚本放在 `/tmp`，**未在仓库内留下任何探针文件**。
- **第四、五次修订**：继续落地代码与仓库治理（`deploy/` 恢复、CI、生产 unwrap 清零、
  `run()` 拆分、TTS 自愈、systemd 停机语义显式化），清单与证明见 §9.4 / §9.5；
  同样未在仓库内留下探针文件。
- **第六次修订**：新增启动自检与健康心跳（`selfcheck.rs`、`poll_until`、`health_check`），
  并顺手修复 http 源被 `-rtsp_transport` 拒绝的缺陷；清单与实测见 §9.6。
  端到端脚本（本地 http 流 + ffmpeg 生成素材）写在**工作区内**、跑完即删，`git status` 收尾为干净。
- **第七次修订**：拆分 `maybe_ocr` 并补 `main.rs` 单测（§9.7）。行为等价性用 `git worktree`
  在拆分前提交上单独编译基线二进制、同场景日志归一化后**逐行比对**证明；临时 worktree
  与测试目录用后即删，`git status` 收尾为干净（仅 `src/main.rs` 一处改动）。
- 所有结论均带 `文件:行号` 证据；测试 / 构建 / clippy / 端到端结果为实跑输出原文。
- 因靶机当前无摄像头设备，**未做真实取流验证**；OCR 识别率结论以项目内既有实测记录
  （`docs/07-troubleshooting.md`、`docs/09-rtsp-field-checklist.md`）为准。
- 第五次修订的"行为等价性"用**同一 simulate 场景、重构前后日志归一化后逐行比对**证明（§9.5）——
  这是无真实取流条件下的替代手段，置信度低于真机回归，如实标注。
- `deploy/` 恢复（§5 第 3 条）已在第四次修订中完成（SSH 通道打通后从 `9892cff` 取回）。

---

## 8. GitHub 远端核对（2026-10-11 补做）

远端仓库：`git@github.com:qianqianjie-999/announcer-ocr.git`（与本地 `anntest-src` 同源）。

| 提交 | 日期 | 说明 |
|---|---|---|
| `b3dd6f6` | 10-08 | chore: 合并 GitHub 远程仓库历史（本地归档 + 远程主线）← **远端 main HEAD** |
| `5c50792` | 10-07 | 新增现场 RTSP 摄像头接入操作清单 ← **本地 HEAD** |
| `6075f3f` | 10-07 | RTSP 源支持加固：修复 ROI 出界 panic，实测端到端链路 |
| `d9946a7` | 10-07 | 初始提交：叫号屏摄像头 OCR 语音播报服务 |
| `b8d5cd0` | 09-29 | chore: 忽略本机推送辅助脚本 |
| `9892cff` | 09-29 | feat: 叫号屏摄像头 OCR 语音播报系统（Python + Rust 双版本）← **完整上游** |

### 结论一：代码没有落后

远端 main HEAD `b3dd6f6` 只比本地多 **1 个提交**，而该提交是**纯历史合并**。
逐项比对远端 main 的 tree 与本地工作区，顶层完全一致：`docs/`、`src/`、`.gitignore`、
`Cargo.lock`、`Cargo.toml`、`README.md`、`config_web.py`（各文件最后一次变更仍停在 10-07）。
→ **本地就是最新内容，`git pull` 不会带来任何代码变化。**

### 结论二：本地工作区只是项目的一部分

被合并进来的 `9892cff`（09-29）才是完整版，其 tree 包含：

| 层 | 内容 |
|---|---|
| 主程序 | `announce.py`（Python 版）、`announce-rs/`（Rust 版） |
| 部署层 `deploy/` | 8 个 `.service`（announce-rs / announce / announce-web / announce-piper / announce-user / worktime-start / worktime-stop / cleanup）、3 个 `.timer`、6 个 `.sh`（install / install_piper_zh / announce-net-apply / announce-cleanup / inst_ocr / install_cleanup）、4 个 `.py`（config_web / tts_server / check_host / config.example） |
| 现场工具 `tools/` | calibrate、autocalib、focus_scan、preview、snapshot、check_resolution |
| 测试 `tests/` | selftest_rs、make_rs_phases、bench_latency（六阶段回放 + 基准） |

**这些文件在 main 的 tree 里已经不存在，只留在历史提交中。**
→ 意味着：新克隆仓库的人**拿不到 systemd 单元、日志治理与工作时段定时器**，
会得到一份"裸的 Rust 程序"。首版评审踩的正是这个坑。

### 结论三：deploy 层里的稳定性设计（值得肯定）

- `announce-rs.service`：`Restart=always` + `RestartSec=5`；`CPUQuota=300%` 且附实测注释
  （"150% 会把这个 cgroup 掐死：98.6% 调度周期被限流，同一张图 tesseract 单独 1.5s、在服务里烧 15s"）、
  `MemoryMax=300M`、`Nice=5`、`CPUWeight=50` —— **资源上限是量出来再定的**，不是拍的。
- `announce-cleanup.sh`：只用明确文件名模式、绝不 `rm -rf` / 通配符横扫；归档只轮转不删除；
  磁盘 <500MB 才激进清理；每轮汇总磁盘 / journal / 内存占用。**这是一份写得很克制的清理脚本。**
- `announce-net-apply.sh` + 90 秒自动回滚：改 IP 改错了会自己弹回来，防"配置完就失联"。
- worktime 定时器：非工作时间自动停服 —— 省电、省 eMMC 寿命，顺带绕开了 `announced` 的长稳问题。

**一句话**：项目的真实工程水平**高于**首版报告给人的印象；扣分主要落在"仓库现状与部署现状不一致"
以及"清理/守护逻辑全在仓库之外"这一点上。建议优先把 `deploy/` 恢复到 main。

### 补充（2026-10-14）

上面这条建议**已执行**：第四次修订即把 `deploy/` 恢复到 main。截至 2026-10-14，
本地与远端 `main` 一致，HEAD = `bdaf754`；其间新增三笔提交：

| 提交 | 日期 | 说明 |
|---|---|---|
| `bdaf754` | 10-14 | 结构重构：拆 `maybe_ocr` 巨石函数，补 `main.rs` 单测并修文档注释错位 |
| `2238db0` | 10-13 | 部署与排查工装入库：裸机部署包（`announce-rs-deploy/`）+ 现场排查脚本（`tools/probe/`） |
| `6676d20` | 10-13 | 现场调优：稳帧再认 + 号码正则锚定 + 按号码去重 |

即"仓库现状与部署现状不一致"这条**已消除**：`deploy/` 在 main，另新增一套裸机部署包。

---

## 9. 优化实施记录（2026-10-11 起，第三~八次修订）

本节记录**已实际落地并经实测验证**的改动。前两版"未修改任何业务源码"的约束在此解除——
本轮的产出就是代码本身。

### 9.1 改动清单

| # | 对应问题 | 改动 | 文件 |
|---|---|---|---|
| 1 | P0-1 无优雅退出 | 新增 `signal.rs`：处理器**只置一个 `AtomicBool`**（异步信号安全），主循环每轮开头检查后 `break`，让正常返回路径触发析构；`PipeGrabber::drop` 重写为「置停 → kill 槽里子进程 → **有界 2s 等抓帧线程自己收完尾**」，并补上"刚 spawn 完就收到停止请求"这个最后的漏点 | `signal.rs`(新)、`main.rs`、`capture.rs` |
| 2 | P0-2 去重集只增不删 | 新增 `dedupe.rs`（`AgedMap` + `Expiry`）：**0=每日本地零点清零（默认）/ 正数=滚动 TTL / 负数=旧行为**，另加硬上限。`announced` 与 `cand_raw`（同样是只增不删的表）一并迁入；主循环每 60s 清理**并打日志**（让隐式维护显形） | `dedupe.rs`(新)、`main.rs`、`cfg.rs` |
| 3 | P1-1 日志无轮转 | `logger::rotate_if_needed`：超阈值把主文件整份改名归档，**归档路径与命名与 `deploy/announce-cleanup.sh` 逐字一致**（`logs_archive/announce_log_%Y%m%d_%H%M%S.csv`、保留 6 个），两侧可共存不冲突；`log_max_mb: 0` 可关闭、完全交回部署层 | `logger.rs`、`cfg.rs` |
| 4 | 启动残留 | 启动清理 `_live/latest.jpg.tmp` 与陈旧 `req`（上次 `kill -9`/掉电的半成品）；**故意不动 `trig_*.jpg`** | `main.rs` |
| 5 | 重启掐断播报 | 新增 `Announcer::drain(timeout)` + `is_idle()`：退出前等队列播完（有界 10s），别把已确认的叫号掐掉 | `announcer.rs` |
| 6 | P2-1 锁 unwrap | 顺带按报告自己的建议，把 `announcer.rs` 的 `lock().unwrap()` 全部换成 `unwrap_or_else(PoisonError::into_inner)`（`panic=abort` 下锁不可能中毒，换掉只是把语义写清） | `announcer.rs` |

新增配置项（全部有默认值，**老 config.json 不用改**）：
`announced_ttl_sec`(0)、`log_max_mb`(8.0)、`log_keep`(6)、`log_archive_dir`("logs_archive")。
新增依赖：`libc = "0.2"`（只为注册信号处理器；libc 本就被 std 链接，不引入任何 C 库依赖，不影响静态交叉编译）。

### 9.2 顺带纠正一条首版错误

首版与第二次修订都写过"存证帧 `_live/trig_*.jpg` 跨重启累积"。核对写盘逻辑后确认**这个说法是错的**：
文件名是 `trig_{:02}.jpg`，即 `trig_01.jpg … trig_20.jpg` 的**固定名**，每次运行从 01 重新覆盖，
目录里永久 ≤20 个文件。所以启动清理**故意不删**它们——它们是上一轮的现场证据；
保留期由部署层按 7 天统一处理。

### 9.3 验证证据

**静态 / 单元测试**（实跑输出）

```
$ cargo test --release
running 38 tests
test result: ok. 38 passed; 0 failed       ← 16 → 38，新增 22 个用例

$ cargo build --release                     ← 0 warning
$ cargo clippy --release                    ← 6 warning, 0 error（与基线同数，未新增）
```

新增覆盖：`signal`(2) / `dedupe`(8，含**"跨天同一号码可重新播报"的回归用例**) /
`logger` 轮转与归档保留(5) / `capture` ROI 裁剪与预览缩图(5) / `announcer` 队列排空(2)

**端到端实测**（跑的是真实二进制、发的是真实信号，不是模拟）

| 用例 | 做法 | 结果 |
|---|---|---|
| A 优雅退出 | simulate 模式跑 22s 后 `kill -TERM` | **退出码 0**，停止耗时 **0.5s**；留痕 `[退出] 收到停止信号(SIGTERM/SIGINT), 开始收尾…` → `[退出] 已优雅停机` |
| A 启动自清理 | 预埋 `_live/req`、`latest.jpg.tmp`、`trig_01.jpg` | 前两者被清理（`[启动] 清理 _live/ 上次遗留的临时文件 2 个`），**`trig_01.jpg` 保留** |
| B 子进程收尸 | RTSP 指向黑洞 IP 让 ffmpeg 常驻，再 SIGTERM | 运行期子进程数 **1** → 退出后 **0**；抓帧线程因管道 EOF 走完 `kill+wait` 收尾 |
| C 维护节拍 | `log_max_mb: 8` + 预填 8.4MB 日志，跑 75s | 第 61 秒真实触发 `[日志] 已达阈值, 轮转 -> logs_archive/announce_log_20261011_212701.csv`，归档 8400037 字节，主文件清空待重建 |

**没验到的部分（如实标注）**：本机 tesseract 对该合成测试卡识别为空串，
所以"真有号码被播报"这条链路没能端到端跑通（因此 `[维护] … 已播报 N 条` 那行日志未在现场出现）。
这是环境相关的既有识别率问题——`ocr.rs` 本轮**一行未改**，`git diff --stat` 可证——
去重表的时间语义由 8 个单测覆盖（含跨天回归用例）。用例 C 已证明维护块本身被按时执行
（轮转就在同一条代码路径里）。

### 9.4 复核后的评分

| 维度 | 权重 | 原(首版) | 三修后 | 五修后 | 六修后 | **七修后** | 变化依据 |
|---|---|---|---|---|---|---|---|
| 故障自愈与容错 | 25% | 9.0 | 9.5 | 9.5 | 9.5 | **9.5** | 信号路径（三修）+ TTS 自愈（五修）+ 首帧就绪轮询（六修）+ 稳帧再认及其 2s 兜底（七修） |
| 错误处理与 panic 防护 | 20% | 8.0 | 8.0 | 9.5 | 9.5 | **9.5** | 五修：生产路径 unwrap/expect/panic 清零（原 15+2+0 处） |
| 长稳与资源治理 | 20% | 7.0 | 8.5 | 9.0 | 9.5 | **9.5** | 六修：健康心跳补上"长期不播报"告警 |
| 可观测性与可排障 | 15% | 9.0 | 9.0 | 9.0 | 9.5 | **9.5** | 六修：启动自检打印依赖版本 + `[健康]` 告警；七修新增 19 个现场排查脚本 |
| 安全与运维 | 10% | 6.0 | 7.5 | 8.0 | 8.0 | **8.5 ↑** | 七修：**部署工装入库**（`--doctor` 体检 / 幂等 / 窄 sudoers / 19 脚本）；扣分在无回滚 |
| 测试与验证 | 10% | 7.0 | 7.5 | 8.0 | 8.5 | **9.0 ↑** | 七修：50 → 63 用例，`main.rs` 0 → 13（稳帧 / 按号码票决 / 影子候选） |
| **加权合计** | | 7.6 | 8.6 | 9.0 | 9.3 | **9.4** | 9.5×.25 + 9.5×.20 + 9.5×.20 + 9.5×.15 + 8.5×.10 + 9.0×.10 = 9.35 |

> 上表「七修后」= **2026-10-14 第七次修订**（现场调优归拢 + 结构重构 + 部署工装）。
> 第四次仅"安全与运维"6.0 → 7.5（总 8.4 → 8.6）；第五次 → 9.0；第六次 → 9.3；第七次 9.3 → **9.4**。
> **第八次修订**（配置台补测试）仅"测试与验证" 9.0 → 9.2，加权 9.37 → 总分仍 **9.4**。

**结论**：程序自身已经能撑住 7×24 连跑，"重启留孤儿 / 跨天吞号 / 写满 SD 卡"三个长稳缺陷
在代码层面全部消除且实测通过。**仓库治理三件事已在第四次修订中处理完毕**（见下）：

1. ✅ **已完成**：`deploy/` 已恢复到 main 分支（2026-10-11，从 `9892cff` 取回 21 个文件）
2. ✅ **已完成**：CI 已落地（`.github/workflows/ci.yml`；其中 `fmt --check` 为非阻塞项）
3. 🚫 **不做**：调试台绑 `127.0.0.1` / 加 token —— 确认内网部署，裸 8080 为有意保留

**升级注意（行为变更）**：`announced` 语义从"永不重复"变为"**当天不重复**"。
现场若确实需要跨天也不重播，把 `announced_ttl_sec` 设为负数即可回到旧行为。

### 9.5 第五次修订（2026-10-11，技术债清偿 + 结构重构）

> 目标：把 P2-1 / P2-2 / P1-2 三条"已知但一直没还"的债一次还清。
> 分两次提交：`09a5905`（P2-1 + P1-2 + systemd）与本轮提交（P2-2）。

**改动清单**

| # | 对应问题 | 改动 | 文件 |
|---|---|---|---|
| 1 | P2-1 生产 unwrap | 生产路径 `unwrap`/`expect`/`panic!` **16 处清零**：`lock().unwrap()` 集中到 `util::lock`（用 `PoisonError::into_inner`）；`now_secs` 墙钟兜底；其余改为错误传播或安全默认值 | `util.rs`(新)、`capture.rs`、`ocr.rs`、`tts.rs`、`detect.rs`、`announcer.rs`、`main.rs` |
| 2 | P1-2 TTS 静默哑掉 | `Announcer::loop_fn` 加失败计数：**连续 3 次播报失败 → 重建引擎**，并打印 `[TTS] 连续 N 次失败, 重建引擎` 留痕 | `announcer.rs` |
| 3 | P2-2 巨石函数 | `run()` 428 行 → `struct Loop`（21 个字段显式声明归属）+ `build`/`run`/`tick`/`diag_heartbeat`/`fast_confirm`/`maybe_ocr`/`maintenance`；**最长函数 436 → 150 行** | `main.rs` |
| 4 | 停机语义 | `deploy/announce-rs.service` 显式写出 `KillMode=control-group` 与 `TimeoutStopSec=15`（不依赖 systemd 默认值，防被后人改坏） | `deploy/announce-rs.service` |

**验证证据**

```
$ cargo test --release
running 43 tests; test result: ok. 43 passed   ← 38 → 43（新增 util 3 + announcer 2）
$ cargo clippy --release -- -D warnings        ← 0 warning（-D warnings 生效）
$ cargo build --release                        ← 0 warning
```

**行为等价性证明（P2-2 拆分的关键，也是本机无摄像头条件下的替代验证）**：
同一 `simulate` 场景、同一 `config.json`，在"重构前二进制"与"重构后二进制"上各跑一遍，
把运行日志归一化（去掉时间戳 / 耗时 / 内存地址）后**逐行 diff，18 行完全一致** ——
证明拆分是纯结构变换、行为零变化。**置信度低于真机回归**（无真实取流），如实标注。

**第五次修订后剩余的两类扣分**：① 第三方库内部 panic 无法兜底（`panic = "abort"` 的明确取舍）；
② `tts` / `main` 集成测试仍为 0（需先解耦出可注入层，即 P1-4 残项）。

### 9.6 第六次修订（2026-10-11，启动自检 + 健康心跳）

> 目标：P2-5 / P2-6 / P2-7 三条"启动健壮性 + 可观测"待办，外加一个顺手发现的真缺陷。

**改动清单**

| # | 对应问题 | 改动 | 文件 |
|---|---|---|---|
| 1 | P2-5 依赖版本无校验 | 新增 `selfcheck.rs`：启动时用 `util::run_timeout` 探测 `ffmpeg -version` / `tesseract --version`，打印 `[自检] <bin> 可用: <首行>` 或缺失告警；只记录、不阻断启动。顺带把 `run_timeout` 从 `tts.rs` 上提到 `util.rs`（播放与自检共用一份线程排空 + try_wait 实现） | `selfcheck.rs`(新)、`util.rs`、`tts.rs`、`main.rs` |
| 2 | P2-6 启动固定 sleep | `capture::open` 的 `sleep(1200ms)` 改为 `poll_until` 轮询首帧（上限 8s）：就绪即走、超时告警后进主循环重试。轮询逻辑抽成接受闭包的纯函数，便于单测 | `capture.rs` |
| 3 | P2-7 无健康探针 | `Loop::health_check`：距上次成功播报 ≥ `health_alert_min`（新配置，默认 60；**0 = 关闭**）分钟即打 `[健康]` 告警，并按同周期限流（防刷屏）。`last_announce` 在两处播报入队点复位 | `main.rs`、`cfg.rs` |
| 4 | 顺带修 bug | `-rtsp_transport tcp` 原先无条件加给**所有** Net 源；而 `cfg.rs` 把 `http(s)://` 也归 Net —— 对 http 源 ffmpeg 会 `Option rtsp_transport not found` 直接拒绝打开输入、**永远取不到帧**。改为仅 `rtsp://` 前缀才加 | `capture.rs` |

**验证证据**

```
$ cargo test --release
running 50 tests; test result: ok. 50 passed   ← 43 → 50（selfcheck 3 + util 2 + capture 2）
$ cargo clippy --release -- -D warnings        ← 0 warning
$ cargo build --release                        ← 0 warning
```

**端到端实测**（真实二进制 + 真实信号）

| 用例 | 做法 | 结果 |
|---|---|---|
| F 启动自检 | simulate 直接启动 | 打印 `[自检] ffmpeg 可用: ffmpeg version 6.1.1-3ubuntu5+esm13 …` 与 `[自检] tesseract 可用: tesseract 5.3.4` |
| G 健康心跳 | `health_alert_min: 0.05`（=3s），屏面无可识别号码，跑 9s 后 SIGTERM | 按 3s 周期打出 `[健康] 距上次成功播报已 0.1 分钟, 进程存活但链路疑似失效 —— 请检查取流(黑屏/失焦/断线)与 OCR; 如需关闭本告警把 health_alert_min 设为 0`；**退出码 0** |
| H 首帧轮询(正向) | 本地 http 流（640×480 mpegts）由 `python3 -m http.server` 提供 | **`[启动] 网络流 首帧就绪`**（在"运行中"之前打印，即就绪后才进主循环）；退出码 0 |
| I 首帧轮询(反向) | `rtsp://10.255.255.1:554/none`（不可达） | **`[启动] 警告: 网络流 等待首帧超时(8s), 先进主循环继续重试`**；退出码 0 |

**没验到的部分（如实标注）**：本机无摄像头/无真实 RTSP 相机，所以"真号码 → 真播报 → 心跳被
复位"这条完整闭环没跑通；心跳**会触发**由用例 G 证明，`last_announce` 的复位点由代码审查确认
（两处 `ann.speak` 的紧邻行）。本机 tesseract 缺 `chi_sim` 语言包，OCR 恒为空 —— 环境问题，
与本次改动无关（`ocr.rs` 一行未改）。

**第六次修订后剩余的两类扣分**（与前次相同的两条）：① 第三方库内部 panic 无法兜底
（`panic = "abort"` 的明确取舍）；② `tts` / `main` 集成测试仍为 0。

### 9.7 第七次修订（2026-10-14，现场调优归拢 + 结构重构 + 部署工装）

> 本次覆盖三笔提交：`6676d20`（现场调优）、`2238db0`（部署工装）、`bdaf754`（结构重构）。

**改动清单**

| # | 类别 | 改动 | 文件 |
|---|---|---|---|
| 1 | 现场缺陷 ① | **稳帧再认**：帧差判变后先静置 `detect.settle_sec`（默认 0.5，钳位 0.1~3.0）再 OCR/存证，掐掉换号滚动/淡入过渡帧的误读（现场实测把 `GA174` 读成 `GAT174`）。**兜底 `SETTLE_MAX_WAIT_SECS`=2.0**：变化持续超 2s 就强制认一次，防"静置窗口被无限续期 → 永不 OCR → 静默不播报" | `main.rs`、`cfg.rs` |
| 2 | 现场缺陷 ② | **号码正则锚定 G 开头** + `ocr.rs::only_digits()` 清洗：`GAT174` 还原 `GA174`，不再回溯切出 `AT174`（字母音读作"诶提"，听感像英语）；数字段被噪声吃光的组直接丢弃，不播半截号 | `ocr.rs` |
| 3 | 现场缺陷 ③ | **去重键由「号码+窗口」改「号码」**，新增 `number_votes()` 按号码聚合投票取窗口（并列取最近）；`fast_confirm` 与正式播报统一走它。修"同一个 GA174 因窗口误读连播 4 遍"。另加 `drop_shadow_empty()` 丢掉"半截话"空窗口候选 | `main.rs` |
| 4 | TTS | `LETTER_CN` 中文谐音（`C`→「西」）+ `spell_window()`：piper 中文模型无声学英语音素，孤立的拉丁字母会被 espeak 按英语音素化而"被吞"，改走中文音素；新增 `speak_window_letter`（默认不念字母） | `tts.rs`、`announcer.rs`、`cfg.rs` |
| 5 | 部署工装 | 新增 `announce-rs-deploy/`（`deploy.sh` 482 行，带 `--doctor` 只体检模式 / 分阶段 / 幂等跳过 / 窄 sudoers）+ `tools/probe/` 19 个现场排查脚本 | 新增 |
| 6 | 结构重构 | `maybe_ocr` 192 行 → 7 个方法（`ocr_due` / `save_evidence` / `recognize_candidates` / `ingest_candidates` / `announce_winners` / `note_ocr_cost` / `note_ocr_failure`）；稳帧判定与按号码票决抽成**可测自由函数** `settle_ready` / `number_votes` | `main.rs` |
| 7 | 补测试 | `main.rs` 新增 `#[cfg(test)] mod tests` **13 用例**：稳帧（2s 兜底 / `change_start` 未初始化 / 动画期不 ready / 已认过不重复存证）、按号码聚合（窗口误读并号 / 不同号不并 / 并列取最近 / 首见取最早）、空窗口影子候选 | `main.rs` |
| 8 | 修缺陷 | `cleanup_live_dir` 的文档注释在上一轮被误接到 `drop_shadow_empty` 头上（前者失注释、后者挂着自相矛盾的说明），本次归位 | `main.rs` |

**验证证据（实跑）**

```
$ cargo test --locked
running 63 tests; test result: ok. 63 passed   ← 50 → 63（main.rs 从 0 → 13）
$ cargo clippy --locked --all-targets -- -D warnings   ← 0 告警
$ cargo build --locked                                 ← 0 告警
```

**行为等价性证明（结构重构的关键，也是本机无摄像头条件下的替代验证）**：
用 `git worktree` 在拆分前的提交 `2238db0` 上单独编译一份基线二进制，与拆分后的二进制在
**同一 simulate 场景、同一 config.json** 下各跑一遍；运行日志归一化（数字替换为 `#`）后
**逐行 diff 完全一致** —— 17 行，含 `[退出] 已优雅停机`，双方退出码 0。
证明这次拆分是纯结构变换、行为零变化。**置信度低于真机回归**（无真实取流），如实标注。

**端到端实测**（真实二进制 + 真实信号）

| 用例 | 做法 | 结果 |
|---|---|---|
| J 冒烟 + 优雅退出 | simulate 跑 10s 后 SIGTERM | `[自检]` 两行 + `运行中:` + `[退出] 已优雅停机`，**退出码 0**，无 panic |
| K 拆分等价性 | `2238db0` 与 `bdaf754` 二进制同场景各跑一遍 | 归一化日志**逐行一致**（17 行，见上） |
| L 单测 | `cargo test --locked` | 63 passed / 0 failed；新增 13 例全部针对现场调优逻辑 |

**没验到的部分（如实标注）**：本机 tesseract 缺 `chi_sim` 语言包
（`/usr/share/tesseract-ocr/5/tessdata/`），OCR 恒为空并打印 `[OCR失败]` —— 环境问题，非本次引入。
真实叫号屏的三个缺陷是**现场机（Debian 13）实测**修复的（提交信息记录了现场生效配置：
`settle_sec=0.5`、`speak_window_letter=false`、`piper_length_scale=1.45`），本轮无法在本机复现真机链路。

**第七次修订后剩余扣分**：① 第三方库内部 panic 无法兜底（`panic = "abort"` 的明确取舍）；
② `tts`（369 行）的**集成**测试仍为 0（`main` 现有 13 单测，覆盖的是抽出来的纯逻辑）；
③ `deploy.sh` 无回滚/卸载路径（P2-9）；④ RTSP 本机判定可绕过（P2-10，已知缺口）。
（原③"`config_web.py` 零覆盖"已在**第八次修订**补上 59 个 Python 用例，见 §9.8。）

---

### 9.8 第八次修订（2026-10-14，配置台补测试）

**背景**：`config_web.py` 是现场唯一的配置改动入口（改摄像头 / ROI / 阈值 / 播报 / 排班都走它），
却 1457 行零自动化覆盖——一条错分支就可能把 `config.json` 写坏，或让"改个阈值把模板清空"重演。

**改动**：新增 `tests/test_config_web.py`，**59 个 `unittest` 用例**（stdlib，零 pip 依赖）。
只测**不碰 IO / 网络 / subprocess** 的纯逻辑，不用 mock 堆"假绿"：

| 被测 | 用例要点 |
|---|---|
| `norm_src` | `/dev/videoN`→`N`、去空白、`/dev/video0/` 不误改、`None`→`""` |
| `validate_rtsp` | 合法（含账号密码/参数）、过长、**伪协议注入（`concat:`/`file:`/`subfile:`/`pipe:`）**、控制字符、非白名单字符（空格/分号）、缺 scheme、本机主机名 |
| `validate_and_build` | 源类型（usb/simulate/rtsp）、分辨率上下界、ROI 长度/负值/越界、`changed_ratio∈(0,1)`、三个时段参数**静默回填**、`settle_sec` 钳到 `[0.1,3.0]`、语速范围/非数字、`ocr.pattern` 缺失保护、排班 `HH:MM` 与起止相同、未编辑字段**原样沿用** |
| `spell_tokens` / `spell_digits` | 数字→中文、`C`→「西」、未列字母原样、去空白；并**断言 Python 映射表与 `tts.rs` 对齐**（防"两处要同步改"漂移） |

**验证证据（实跑）**

```
$ python3 -m unittest discover -s tests -v
Ran 59 tests in 0.006s
OK
```

**变异测试（证明不是空跑）**——故意打破被测函数，"应当红"的用例必须红：

| 变异 | 期望 | 实测 |
|---|---|---|
| 伪协议黑名单去掉 `file:` | 注入用例红 | ✅ 红 1 |
| `validate_rtsp` 恒返回 `(True, src)` | 校验用例大面积红 | ✅ 红 7（另 `build.rtsp_invalid` 红 1） |
| `spell_tokens`/`spell_digits` 退化为原样 | spell 用例红 | ✅ 红 6 |
| 还原以上三处 | 全绿 | ✅ 红 0 |

**走查中登记的已知缺口（未改代码）**：RTSP「不允许指向本机」（`config_web.py:330`）只枚举
`localhost / 127.0.0.1 / 0.0.0.0 / ::1` 四个字面值，`rtsp://[::1]/s`（IPv6 回环方括号写法，
host 解析成 `[`）与 `rtsp://127.1/s`（127.0.0.0/8 简写）仍可通过。危害有限（"读本机文件"已被
伪协议黑名单拦死），属纵深防御漏网，登记为 P2-10。`test_loopback_variants_known_gap` 以用例
固化现状，修复后应改为断言拒绝。

**本次性质**：**只新增测试与文档，未改任何业务源码**（`git diff` 可证）。
