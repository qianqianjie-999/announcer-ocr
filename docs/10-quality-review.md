# announce-rs 生产级稳定性评审

> 评审对象：本仓库（叫号屏摄像头 OCR 语音播报 · 无人值守服务）
> 评审口径：**聚焦"生产级稳定性"**（7×24 无人值守场景下的容错、自愈、长稳、可排障），
> 安全与运维作为边界项一并列出。
> 评审方式：静态走读 + 客观工具输出 + 单元测试实跑。**首版与第二次修订未修改任何业务源码。**
> **第三次修订（2026-10-11）性质不同——它记录的是按本报告落地并实测验证过的优化**，见 §9。

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

**评级：A-（8.6 / 10，第四次修订后）—— "免运维 7×24" 的三个必修项已全部补齐。**

> 首版 7.6 → 补上 deploy 层事实 7.9 → 优化落地 8.4 → **仓库治理补齐 8.6**（明细见下表与 §9.4）。
> 8.6 分的含义：程序自身能撑住 7×24 连跑；仓库治理三项中前两项已解决、第三项经确认为内网有意设计。
> **剩余扣分全是"可以慢慢还"的技术债，没有止血项。**

这是一份工程成熟度明显高于同类脚本项目的代码。稳定性设计的**密度**是最大亮点：
每一个子进程都有超时、每一条自愈路径都有退避、每一个已知踩坑都在代码注释里写清了根因和实测数据
（"靶机实测 1.43s""圈速 24s > TTL 4s"这种量级）。38 个单元测试全绿，clippy 0 error。

**首版指出的三处长稳缺陷，现已全部修掉并实测**（详见 §9）：进程收到 SIGTERM 会先播完队列
再收掉 ffmpeg 子进程（不再留孤儿）、去重表带时间维度每天零点清零（不再跨天吞号）、
日志写满 8MB 自己归档（不再依赖部署层第二天来收）。

| 维度 | 权重 | 得分 | 判断 |
|---|---|---|---|
| 故障自愈与容错 | 25% | 9.5 ↑ | 子进程全超时 + 断流自愈 + 收尸 + 退避；**新增信号处理，收尸不再依赖 systemd** |
| 错误处理与 panic 防护 | 20% | 8.0 | Result 传播干净，边界防御有回归测试。但 `panic=abort` + 15 处生产 unwrap、无 `catch_unwind` |
| 长稳与资源治理 | 20% | 8.5 ↑ | 去重表有界 + 日志自轮转 + 启动自清理 + 60s 维护节拍；仍缺"长期不播报"健康告警 |
| 可观测性与可排障 | 15% | 9.0 | 监测心跳 / 触发存证 / 延迟打点 / CSV 留痕 / 10 篇文档；新增 `[维护]`/`[日志]`/`[退出]` 留痕 |
| 安全与运维 | 10% | 7.5 ↑ | `deploy/` 回到 main + CI 落地 + clippy 清零；8080 经确认为内网有意设计；窄 sudoers + 改 IP 90s 回滚 |
| 测试与验证 | 10% | 7.5 ↑ | 用例 16 → 38 全绿且是真逻辑断言；capture/announcer 从 0 覆盖；tts/main 仍 0 |
| **加权合计** | | **8.6** | （9.5×.25 + 8.0×.20 + 8.5×.20 + 9.0×.15 + 7.5×.10 + 7.5×.10 = 8.55） |

---

## 2. 客观指标（实测）

> 口径：`生产代码` = 各文件 `#[cfg(test)]` 之前的全部内容（单测里的 unwrap 不计）。
> 基线数值由 `git show HEAD:src/<file>` 用同一脚本重测，保证可比。

### 规模

| 文件 | 行数 | 生产代码 unwrap/expect | 单测数 | 本次是否改动 |
|---|---|---|---|---|
| main.rs | 510 | 1 / 0 | 0 | 改（维护节拍 / 启动清理 / 优雅退出） |
| capture.rs | 475 | 6 / 1 | 5 | 改（收尸三段式 + 补 5 个用例） |
| ocr.rs | 412 | 2 / 0 | 9 | 未动 |
| tts.rs | 381 | 2 / 0 | 0 | 未动 |
| detect.rs | 232 | 0 / 1 | 5 | 未动 |
| logger.rs | 235 | 0 / 0 | 7 | 改（归档轮转 + 补 5 个用例） |
| dedupe.rs | 226 | 0 / 0 | 8 | **新增** |
| announcer.rs | 210 | 2 / 1 | 2 | 改（停机排空 + 锁不 unwrap） |
| cfg.rs | 170 | 0 / 0 | 0 | 改（4 个新配置项） |
| signal.rs | 66 | 0 / 0 | 2 | **新增** |
| **合计** | **2917** | **13 / 3** | **38** | 基线：2169 / 15 / 3 / 16 |

### 工具输出原文

```
$ cargo test --release
running 38 tests
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured  ← 16 → 38，全绿

$ cargo build --release
（无输出）                                                   ← 0 warning（含新增依赖 libc）

$ cargo clippy --release
warning: `announce-rs` (bin "announce-rs") generated 0 warnings ← 第四次修订已清零（原先 6 条）
  io_other_error ×2 / needless_range_loop ×2 / collapsible_str_replace / len_zero

$ cargo fmt --all --check
956 行差异                                                    ← 未适配 rustfmt（手工对齐排版），见 P2-3

$ git ls-files | grep -E '\.key$|\.pem$|\.env$|config\.json$'
无                                                           ← 无凭据/配置入库
```

其他信号：`unsafe` **1 处**（`signal.rs` 注册信号处理器，唯一一处，带 SAFETY 注释并说明为何
不直接 `fn_item as usize`）、`TODO/FIXME` 0 处、`#[allow(...)]` 0 处、
生产路径 `panic!` 0 处、`Command::new` 6 处（全部带超时或自愈）。

**最长函数**：`main.rs:47 run()` 436 行（占该文件 85%，内嵌 5 层，平铺 25+ 个状态变量）——
比基线 362 行更长，因为本轮把维护节拍与启动自清理也放在这里。**P2-2 的拆函数建议比之前更紧迫**。

---

## 3. 做得好的地方（建议不要动）

1. **子进程全部带超时 + 管道排空**。`ocr.rs:89` tesseract 10s 上限、`tts.rs:19` 通用 `run_timeout`、
   `tts.rs:239` piper 180s、aplay 120s。并且用独立线程 `read_to_end` 排空 stdout/stderr 防写满阻塞。
   → 一个挂死的子进程**不会**卡死主循环。这是很多生产服务都没做到的一条。
2. **ffmpeg 断流自愈**。`capture.rs:54-157` 管道断开 → `kill + wait` 收尸（防 defunct 僵尸）→ 3s 后重拉；
   重拉失败 5s 退避重试，直到 Drop。注释里记着"13:21 实测踩坑"。
3. **输入防御有回归测试兜底**。`detect.rs:16` ROI 出界返回 0×0 空图跳帧而非数组越界 panic，
   配套 `detect.rs:181` 用例覆盖"完全出界 / 半出界 / 尺寸恢复自愈"四种情形——这是 RTSP 降流炸过之后补的。
4. **主循环无阻塞确认状态机**。`main.rs:70-72` 不在确认期 sleep，避免"屏幕连续跳号被盲区吞掉"；
   `main.rs:329` 有效 TTL 自适应为 `max(配置, 3×本轮OCR耗时)`，直接解决了"圈速 24s > TTL 4s → 永不播报"。
5. **自适应退避，防静止画面空烧 CPU**。`main.rs:78-89` 空转退避（翻倍至 30s 封顶）+ 静帧兜底放宽到 10s +
   静帧快速确认免掉第二轮 OCR（把 1.6~4.8s 的单项延迟从关键路径上摘掉）。
6. **日志/预览失败绝不影响主流程**。`logger.rs:29-55` 重试 3 次（0.3s/0.6s）后丢弃并告警；
   `capture.rs:256-266` 预览原子写（tmp+rename）+ `try_lock` 串行化，防半截图与 ENOENT 刷屏。
7. **可观测性完备**。监测心跳（近 N 秒最大帧差 + OCR 耗时统计）、触发帧存证（看画面直接定位）、
   `[时序] 首见→播报 ms` 延迟打点、utf-8-sig CSV（Excel 直开）。现场排障基本靠这几样就能闭环。

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

**P1-1　日志轮转与磁盘治理：代码侧确实没有，但部署侧已经补上了**
> **状态：🟡 代码侧已补（2026-10-11）。** 新增 `logger::rotate_if_needed`，归档路径与命名
> 与清理脚本**逐字一致**，两侧可共存；默认 8MB / 保留 6 个，与脚本阈值对齐。
> 实测在第 61 秒真实触发轮转（§9.3 用例 C）。**残留风险未变**：`deploy/` 仍不在 main 分支，
> 新克隆者拿不到 systemd 单元与清理定时器（见 §5 第 3 条）。
> **修正**：首版判"无轮转、会写满 SD 卡"。远端 `deploy/announce-cleanup.sh` + `announce-cleanup.timer`
> （每日 03:30）实际上是一套完整的磁盘治理：CSV 超 8MB 轮转并保留最近 6 个归档、
> `trig_*.jpg` 只留 7 天、清 `latest.jpg.tmp` 与超 1 小时的陈旧 `req`、`.bak.*` 留 30 天、
> `journalctl --vacuum-size=64M`、可用空间 <500MB 触发激进清理。**这条基本已被解决。**

- 残留风险：该治理**全部在仓库 tree 之外**——`deploy/` 只存在于历史提交 `9892cff`，
  当前 main 分支 tree 里已没有它（见 §8）。也就是说：脚本存在，但**仓库现状不保证它被安装**。
  清理逻辑与主程序也是解耦的（清的是固定路径 `/home/debian/anntest`），换部署路径即失效。
- 修复：把 `deploy/` 恢复到 main 分支；清理脚本的 `DIR` 改为从 systemd 环境或参数注入。

**P1-2　TTS 引擎只在启动时构建一次，失效后静默哑掉**
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
- 证据：单测分布见 §2——`capture.rs`(364) / `tts.rs`(381) / `announcer.rs`(140) / `main.rs`(412) 全部为 0；
  仓库内无 `.github/`、无 Makefile、无任何自动化脚本。
- 后果：这三块恰好是"最容易出稳定性问题"的部分（子进程、线程、状态机），回归完全依赖人工靶机实测。
- **已解决（2026-10-11）**：新增 `.github/workflows/ci.yml` —— `build --locked` + `test`
  + `clippy -D warnings`（rustfmt 检查为非阻塞）；clippy 告警已清零，`-D warnings` 因而可直接生效。
- 仍待办：抽一层可注入的 `CommandRunner` trait，把 `capture`/`tts` 的超时与重试路径变成可单测的。

### P2 — 技术债

| # | 问题 | 位置 | 建议 |
|---|---|---|---|
| P2-1 | `panic = "abort"` + 15 处生产 `unwrap()` + 2 处 `expect`，无 `catch_unwind` | `Cargo.toml:14`、`capture.rs:80/99/101/143/160/177`、`tts.rs:22-23/230`、`main.rs:26`、`ocr.rs:106-107`、`detect.rs:159` | 任一 panic 直接杀进程（靠 systemd 拉起尚可接受）。优先消掉 mutex `lock().unwrap()`，换 `unwrap_or_else(PoisonError::into_inner)` |
| P2-2 | `run()` 单函数 362 行，5 层嵌套、20+ 平铺状态变量 | `main.rs:44-404` | 抽 `ConfirmState` / `StaticGuard` 结构体，把状态机从主循环里分出来 |
| P2-3 | 未适配 rustfmt（758 行差异） | 全库 | 接受现状（对齐排版可读性更好）或跑一次 `cargo fmt --all` 统一 |
| P2-4 | ~~6 条 clippy 风格告警~~ ✅ **已清零**（含测试代码共 10 条） | `ocr.rs:168`、`detect.rs:133/137`、`capture.rs:321`、`main.rs:315/336` | 已手工修复：`io::Error::other` / 索引循环改迭代器 / `replace([..])` / `is_empty()` |
| P2-5 | 外部二进制（ffmpeg / tesseract / aplay / piper）无版本固定与启动校验 | `capture.rs:165`、`ocr.rs:90`、`tts.rs:83` | 启动时 `--version` 探测并记入日志，便于"换个靶机就变慢"这类问题定位 |
| P2-6 | 启动固定 `sleep 1200ms` 等首帧，无就绪校验 | `capture.rs:339`、`capture.rs:355` | 改为轮询首帧到达（带超时），省启动时间且更可靠 |
| P2-7 | 无健康探针：进程活着但"长期不播报"这类软故障无外显告警 | 全库 | 加"距上次成功播报 > N 分钟"心跳告警 |
| P2-8 | `config_web.py` 61KB 单文件、零测试 | 全库 | 与主服务解耦，至少给 `validate_rtsp` / 配置归一化补单测 |

---

## 5. 改进路线

> ✅ = 2026-10-11 已落地并通过实测（详见 §9）；⬜ = 待办。

**天级止血（上线前）**
1. ✅ **代码侧**：SIGTERM/SIGINT 处理器 + 收尸闭环（`src/signal.rs`、`PipeGrabber::drop`）；
   ⬜ **部署侧**：`deploy/announce-rs.service` 仍建议把 `KillMode=control-group` 与
   `TimeoutStopSec=10` 显式写出来（不依赖默认值，防后人改坏）
2. ✅ `announced` 改为带时间维度的 `AgedMap`：默认每日本地零点清零，与 worktime 定时器解耦，
   可安全 7×24 连跑；另加 4096 条硬上限
3. ✅ **已把 `deploy/` 恢复到 main 分支**（2026-10-11：SSH 通道打通后从 `9892cff` 取回全部 21 个文件；
   README 新增「部署（deploy/）」一节说明各单元用途）

**周级补齐**
4. ⬜ TTS 失败计数 → 重建引擎 + 告警（P1-2）；本轮只做了"停机前把队列播完"（`Announcer::drain`）
5. 🚫 **不做**：调试台绑 `127.0.0.1` —— 确认部署于内网，裸 8080 调试为有意保留（P1-3 已改判）
6. ✅ **已加 GitHub Actions**（2026-10-11）：`.github/workflows/ci.yml` 跑 `build --locked` + `test`
   + `clippy -D warnings`；rustfmt 非阻塞
7. 🟡 部分完成：`logger` / `dedupe` / `signal` / `capture` 的临界逻辑已补 22 个用例；
   `tts`/`main` 仍需解耦出可测层

**季度结构性整理**
8. ⬜ 拆 `run()` 巨石函数（P2-2）—— 本轮变长到 436 行，优先级应上调
9. ⬜ 外部依赖版本探测 + 启动自检（P2-5）
10. ⬜ "距上次播报"健康心跳（P2-7）

---

## 6. 附录：复现命令

```bash
export RUSTUP_HOME=$HOME/.rustup CARGO_HOME=$HOME/.cargo
export PATH=$HOME/.cargo/bin:$PATH
cd <仓库根>                    # 本机为 ~/OCR/anntest-src

cargo test --release          # 16 passed; 0 failed
cargo clippy --release        # 6 warnings, 0 error
cargo fmt --all --check       # 758 行差异
git ls-files                  # 无凭据/配置入库

grep -rn "signal\|SIGTERM\|SIGINT\|ctrlc" src/    # 0 命中 → P0-1
grep -n "announced" src/main.rs                   # 只有 insert，无 retain/clear → P0-2
```

---

## 7. 评审纪律说明

- **第一、二次修订**：全程**未修改任何业务源码**；未新增临时探针文件。
- **第三次修订**：按本报告落地优化，**改动了业务源码**（清单见 §9.1），并新增 2 个模块
  与 22 个用例。端到端实测脚本放在 `/tmp`，**未在仓库内留下任何探针文件**。
- 所有结论均带 `文件:行号` 证据；测试 / 构建 / clippy / 端到端结果为实跑输出原文。
- 因靶机当前无摄像头设备，**未做真实取流验证**；OCR 识别率结论以项目内既有实测记录
  （`docs/07-troubleshooting.md`、`docs/09-rtsp-field-checklist.md`）为准。
- 本机网络直连 `github.com` 超时，远端核对走网页抓取；因此 `deploy/` 的恢复（§5 第 3 条）
  本轮未能完成。

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

---

## 9. 优化实施记录（2026-10-11 第三次修订）

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

| 维度 | 权重 | 原 | 现 | 变化依据 |
|---|---|---|---|---|
| 故障自愈与容错 | 25% | 9.0 | **9.5** | 信号路径补齐，收尸闭环不再依赖 systemd 的 cgroup 兜底 |
| 错误处理与 panic 防护 | 20% | 8.0 | 8.0 | 未动（`panic=abort` 与剩余 13 处 unwrap 仍在） |
| 长稳与资源治理 | 20% | 7.0 | **8.5** | 去重表有界、日志自轮转、启动自清理、60s 维护节拍；仍缺"长期不播报"健康告警 |
| 可观测性与可排障 | 15% | 9.0 | 9.0 | 新增 `[维护]`/`[日志]`/`[退出]`/`[启动]` 留痕，隐性维护变显性 |
| 安全与运维 | 10% | 6.0 | **7.5** | 第四次修订：`deploy/` 回 main、CI 落地、clippy 清零；8080 经确认为内网有意设计 |
| 测试与验证 | 10% | 7.0 | **7.5** | 16 → 38 用例，capture/announcer 从 0 覆盖；tts/main 仍为 0 |
| **加权合计** | | 7.9 | **8.6** | 9.5×.25 + 8.0×.20 + 8.5×.20 + 9.0×.15 + 7.5×.10 + 7.5×.10 = 8.55 |

> 上表「现」列 = **第四次修订后**（第三次修订时为 8.4）。第四次仅"安全与运维"由 6.0 提到 7.5，
> 其余维度不变；总加权 8.4 → **8.6**。

**结论**：程序自身已经能撑住 7×24 连跑，"重启留孤儿 / 跨天吞号 / 写满 SD 卡"三个长稳缺陷
在代码层面全部消除且实测通过。**仓库治理三件事已在第四次修订中处理完毕**（见下）：

1. ✅ **已完成**：`deploy/` 已恢复到 main 分支（2026-10-11，从 `9892cff` 取回 21 个文件）
2. ✅ **已完成**：CI 已落地（`.github/workflows/ci.yml`；其中 `fmt --check` 为非阻塞项）
3. 🚫 **不做**：调试台绑 `127.0.0.1` / 加 token —— 确认内网部署，裸 8080 为有意保留

**升级注意（行为变更）**：`announced` 语义从"永不重复"变为"**当天不重复**"。
现场若确实需要跨天也不重播，把 `announced_ttl_sec` 设为负数即可回到旧行为。
