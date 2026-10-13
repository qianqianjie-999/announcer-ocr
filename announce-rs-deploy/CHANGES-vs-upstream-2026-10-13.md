# 代码版本对比：现场部署版 vs GitHub 上游

> **历史记录（2026-10-13）**：本文写于"要不要升级到上游"的决策时点，当时现场跑的是
> `b3dd6f6e`（落后上游 5 个提交）。
> 结论已执行：现场已升级到 `fc4d64d`，并在其上叠加了现场调优补丁
> （稳帧再认 / 号码正则锚定 / 按号码去重，见同日提交），所以文中"落后 5 个提交"是当时状态。

- **仓库**：`qianqianjie-999/announcer-ocr`（main 分支，public）
- **上游最近推送**：2026-10-11 14:32 UTC
- **对比时间**：2026-10-13
- **本地基线**：`b3dd6f6e`（2026-10-08，即现场正在跑的那份）
- **上游最新**：`fc4d64dc`（2026-10-11）

---

## 一、结论速览

| 项目 | 结果 |
|---|---|
| 落后提交数 | **5 个**（集中在 2026-10-11 一天内） |
| 源码规模 | 2161 行 → **3427 行**（+1266，+59%） |
| 新增源文件 | `dedupe.rs` / `selfcheck.rs` / `signal.rs` / `util.rs` |
| 新增依赖 | `libc = "0.2"` → 解析为 **0.2.189**（本地 `vendor/` 与 `crates-cache/` **已有**，离线可编） |
| 配置兼容性 | ✅ 新字段全有默认值，老 `config.json` **不用改** |
| 是否建议升级 | **建议升级** —— 其中 1 条是「7×24 连跑必踩」的真 bug |

---

## 二、上游 5 个提交

| SHA | 提交 | 性质 |
|---|---|---|
| `97e06247` | 长稳加固：优雅退出、去重集按天治理、日志轮转 | 🔴 修 bug |
| `d676df39` | 补齐仓库治理：恢复 `deploy/`、加 CI、clippy 清零 | 🟡 工程 |
| `09a59053` | 技术债清偿：清零生产 `unwrap`、TTS 引擎自愈、停机语义显式化 | 🟡 加固 |
| `03cdfd73` | 结构重构：`run()` 拆为 Loop 状态机，评审文档同步至第五次修订 | 🟡 重构 |
| `fc4d64dc` | 启动自检 + 健康心跳：依赖版本探测、首帧就绪轮询、长稳告警 | 🟢 新功能 |

---

## 三、文件级差异（本地 → 上游）

| 文件 | 新增 | 删除 | 说明 |
|---|---:|---:|---|
| `src/main.rs` | +569 | −298 | 重构为 Loop 状态机 + 启动清理 + 去重表维护 |
| `src/capture.rs` | +214 | −21 | `PipeGrabber::drop` 重写：置停 → kill → 有界 2s 收尾 |
| `src/logger.rs` | +161 | −1 | 按大小轮转，归档命名与 `deploy/announce-cleanup.sh` 一致 |
| `src/announcer.rs` | +149 | −12 | `drain(10s)` / `is_idle()`，退出前排空队列 |
| `src/cfg.rs` | +24 | 0 | 新增 4 个配置项（全带默认值） |
| `src/ocr.rs` | +13 | −5 | — |
| `src/detect.rs` | +12 | −7 | — |
| `src/tts.rs` | +2 | −43 | 引擎自愈，删掉冗余回退 |
| `Cargo.toml` | +3 | 0 | 加 `libc = "0.2"` |
| `Cargo.lock` | +1 | 0 | 仅登记 libc 为直接依赖 |
| `README.md` | +47 | −3 | 新增「停机、重启与长稳维护」章节 |
| `docs/07-troubleshooting.md` | +31 | −2 | — |
| `docs/06-config.md` | +4 | 0 | — |
| **新增** `src/dedupe.rs` | 225 | — | 带去重时间治理的表 |
| **新增** `src/util.rs` | 147 | — | 不中毒取锁 / 带超时子进程 |
| **新增** `src/selfcheck.rs` | 72 | — | 启动自检 |
| **新增** `src/signal.rs` | 65 | — | SIGTERM/SIGINT 优雅退出 |
| **新增** `deploy/`（21 个文件） | — | — | systemd 单元 + 安装/清理脚本 |
| **新增** `.github/` | — | — | CI |
| **新增** `docs/10-quality-review.md` | 538 | — | 生产级稳定性评审（六次修订） |

---

## 四、对本现场的实质影响（按优先级）

### ★ P0：去重表跨天不清理 → 号码复用被静默哑掉（现场正在踩）

**上游修复**：新增 `dedupe.rs`，`announced` 去重表默认**每日本地零点清零**（`announced_ttl_sec: 0`），
并每 60 秒清理一次且打日志 `[维护] 去重表清理`。

**原实现的缺陷**（上游文档原话）：`announced` 只 insert 不删除，而叫号号码每天循环复用；
进程连续跑过几天后，与前几天重复的 `(号码, 窗口)` 会被**永久判为「已播报」而静默不再出声**，
且不产生任何告警。现场没暴露只是因为部署层的 worktime 定时器每天 18:00 停服「顺带」清空了集合
——**改成 7×24 连跑立刻显形**。

**现场证据（本次实测）**：

| 观测 | 数据 |
|---|---|
| 服务进程 | MainPID `692`，启动于 `2026-10-13 15:31:19`（开机 41 分钟） |
| 日志总行数 | 1818 |
| 真正播报（OK）| 360 条，首条 `10-10 18:37`，末条 `10-13 16:11` |
| 相同 `(号码,窗口)` 播报 ≥2 次 | **31 组** |
| 这 31 组的第二次播报时刻 | **全部落在 15:31:19 重启之后** |

→ 说明去重表是**纯内存态、只靠进程重启清空**。而现场 `schedule.enabled=false`（全天运行、
**没有每日 18:00 重启**），此前已连续运行约 2.8 天（10-10 18:38 → 10-13 15:26）。
**只要继续 7×24 跑，号码一旦跨天复用就会被静默跳过** —— 这正是上游要修的软故障。

### P1：重启留孤儿 `ffmpeg`，占住摄像头

原实现收到 `SIGTERM`（`systemctl restart`、配置台「保存」都会触发）时立即终止，
**析构函数不执行**，而 kill+wait 收尸只写在 `PipeGrabber::drop` 里 → 每次都把 `ffmpeg`
留成孤儿继续占设备，新实例只能靠 5s 重试硬扛。上游新增 `signal.rs`（只置一个
`AtomicBool`，异步信号安全）+ `capture.rs` 重写 drop 修复。

### P1：播报日志无轮转

原实现只增不减。上游加 `log_max_mb`（默认 8MB）+ `log_keep`（默认 6），
归档路径/命名与 `deploy/announce-cleanup.sh` **逐字一致**，两层可共存。

### P2：重启掐断正在播的叫号

上游新增 `Announcer::drain(10s)`：退出前等队列播完，不把已确认的叫号掐掉。

### P2：启动自检 + 健康心跳（新功能）

- `selfcheck.rs`：启动时探测 `ffmpeg` / `tesseract` 版本并落日志，现场排障一眼对上号。
- `health_alert_min`（默认 60 分钟）：距上次成功播报超过阈值就打印 `[健康]` 告警，
  用于发现「进程活着、链路已死」的静默软故障。**0 = 关闭**（本现场午休/夜班长时间无人叫号，
  如觉得告警吵可设为 0 或调大）。

### P3：`deploy/` 运维即代码（21 个文件）

`announce-rs.service`（`Restart=always` / `CPUQuota=300%` / `MemoryMax=300M`）、
`announce-piper.service`（常驻 piper）、`announce-cleanup.{service,timer,sh}`（**每日 03:30**
清理日志与过期存证）、`install_cleanup.sh`、`inst_ocr.sh`、`check_host.py`、`config.example.py` 等。
> 其中 **03:30 日志清理**正是此前一直欠着的那项待办。

---

## 五、我们本地独有的改动（升级时必须保留）

| # | 内容 | 位置 | 上游是否有 |
|---|---|---|---|
| 1 | **工作时间调度** Web UI（启用勾选 + 起止时间 + 状态提示，`s_en/s_start/s_end`） | `config_web.py` | ❌ 无 |
| 2 | 保存后调 `SCHED_CMD` 应用调度；`schedule` 段写盘 | `config_web.py` | ❌ 无 |
| 3 | **测试播报用页面当前填写值**（本次新加，`repeat/rate/spell_number` override） | `config_web.py` | ❌ 无 |
| 4 | `/usr/local/sbin/announce-schedule-apply`（读 `schedule` 段生成/清理 timer） | 目标机 | ❌ 无（上游是硬编码 timer） |
| 5 | sudoers 窄授权（`announce-schedule`、`announce-web`） | 目标机 | ❌ 无 |
| 6 | `announce-rs-deploy/deploy.sh` + README（一键离线部署） | 本地 | ❌ 无 |
| 7 | 日志路径对齐（配置台读 `config.json` 的 `log_file`） | `config_web.py` | ❌ 无 |

**补丁量**：`config_web.py` 相对上游 = **+106 / −14 行**。

> ✅ **好消息**：上游这 5 个提交**完全没有改 `config_web.py`**（上游根目录那份与我们的
> `config_web.py.orig` **逐字节相同**）。所以升级时我们这份 `config_web.py` **不需要重新打补丁**，
> 直接原样沿用即可 —— 它已经等于「最新上游 + 我们的补丁」。
> （注意：`deploy/config_web.py` 是 2026-09 的历史快照，官方 README 明确说**以根目录那份为准**。）

---

## 六、冲突点：工作时间调度两套方案

| | 上游 `deploy/announce-worktime-*.timer` | 我们的 `announce-schedule-apply` |
|---|---|---|
| 起止时间 | **硬编码 08:00 / 18:00** | **Web 页面可配**（含跨午夜，如 22:00~06:00） |
| `Persistent` | `true`（错过的触发点会**补跑**） | `false`（**不补跑**，由开机服务统一校正） |
| 半夜拉起风险 | ⚠️ 有（机器在非工作时段开机会把当天错过的 start 补跑） | ✅ 无 |
| 适用性 | 标准作息 | **本现场要求 Web 可配 → 保留我们的方案** |

→ **建议**：升级二进制时**不要照搬**上游这对 timer，继续用我们的 `announce-schedule-apply`。
两者互不干扰（上游 Rust 代码根本不读 `work_start/work_end`，工作时间完全由部署层实现）。

---

## 七、升级路径建议

1. **以仓库根目录为新基线**覆盖 `src/`、`Cargo.toml`、`Cargo.lock`、`docs/`、`README.md`；
   **保留**本地 `config_web.py`（含我们的补丁）与 `config_web.py.orig`。
2. **离线编译**：无需联网补包 —— 唯一新依赖 `libc 0.2.189` 已在 `vendor/` 与 `crates-cache/`。
   `cargo build --release --offline -j2` 即可（预计仍是一分钟量级）。
3. **部署新二进制**到 `/opt/announce-rs/announce-rs`，重启 `announce-rs.service`。
4. **`config.json` 不用改**（新字段全有默认值）。但建议确认这几点：
   - `announced_ttl_sec`：**留空/0** 即「每天零点清零」，正是本现场需要的；
   - `health_alert_min`：默认 60 分钟告警；若午休/夜班长时间无叫号嫌吵，设 `0` 关闭；
   - `log_max_mb` / `log_keep`：默认 8MB / 留 6 个，可直接用。
5. **可选采纳** `deploy/announce-cleanup.{service,timer,sh}`（每日 03:30 清理）——
   补上欠了 3 天的待办；但脚本内 `DIR=/home/debian/anntest` 需改成 `/opt/announce-rs`。
6. **不要采纳** `deploy/announce-worktime-*.{service,timer}`（与我们的 Web 可配方案冲突）。

---

## 八、不需要做的事

- ❌ 不需要重新给 `config_web.py` 打补丁（上游未改它）。
- ❌ 不需要新增任何 crate / 联网。
- ❌ 不需要修改 `config.json`（向后兼容）。
- ❌ 不需要动 `schedule` 段（上游 Rust 代码仍会忽略它）。

---

## 附：本次对比所用命令

```bash
# 上游可达性（git 协议被网络拦，走 REST API + codeload）
curl -s https://api.github.com/repos/qianqianjie-999/announcer-ocr
curl -sL -o up.tar.gz \
  https://codeload.github.com/qianqianjie-999/announcer-ocr/tar.gz/refs/heads/main

# 逐文件差异
diff -rq --exclude=__pycache__ --exclude=target src/announcer-ocr-main upstream/announcer-ocr-main
git diff --no-index --numstat --no-renames <本地文件> <上游文件>
```
