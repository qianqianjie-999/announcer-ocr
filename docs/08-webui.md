# 08 · Web 配置调试台（config_web.py）

## 定位

现场调试/标定的浏览器操作台，与 `announce-rs` 主服务同机部署、同一目录
（要读写 `config.json`/`announce_log.csv`/`sim_frame.jpg`）。

- **Python 标准库实现，零 pip 依赖**——弱靶机不用装任何包
- 端口 `8080`，**仅限内网、无认证**（调试场景定位，勿暴露公网）
- 启动：`python3 config_web.py`，浏览器开 `http://<靶机IP>:8080/`

## 功能页

| 功能 | 说明 |
|---|---|
| 配置编辑 | 摄像头源(USB/RTSP 含 IP+密码)/分辨率/检测参数/播报参数 |
| **ROI 拖框** | 预览图上鼠标拖矩形，坐标按「配置的采集分辨率」自动换算（`capW()`），标定 ROI 不用手工算坐标 |
| 抓帧预览 | `/api/snapshot`：ffmpeg 对 USB/RTSP 抓一帧 JPEG（超时 15s），失败给中文错误（IP/密码/网络可达） |
| 一键保存 | 校验 → **原子写** config.json(tmp+rename) → `sudo systemctl restart announce-rs` |
| 服务状态 | systemd ActiveState/PID/内存 + 实时播报日志 |
| OCR 自测 | `/api/test_ocr`：当前配置跑一轮识别，直接看解析结果 |
| TTS 自测 | `/api/test_tts`：合成"GA200→08号窗口"试听，piper 走常驻服务优先 |
| 音频修复 | `/api/audio_fix`：音频设备异常时的诊断/修复动作 |
| USB 设备枚举 | `/api/devices`：列出可用摄像头 |
| 日志管理 | `/api/logs`：查看/清理播报 CSV |
| 网络配置 | `/api/network`：配 RTSP 相机 IP 等，经 `announce-net-apply.sh` 应用，**二次确认**后生效 |

## API 一览

```
GET  /                     配置页 HTML
GET  /api/config           读配置
GET  /api/status           服务状态+播报日志
GET  /api/devices          USB 摄像头列表
GET  /api/audio            音频设备状态
GET  /api/network          网络状态
GET  /api/logs             日志状态
GET  /api/snapshot?...     抓帧预览(JPEG)
POST /api/network[/confirm] 网络配置应用(带确认)
POST /api/logs             日志操作
POST /api/test_ocr         OCR 自测
POST /api/test_tts         TTS 试听
POST /api/audio_fix        音频修复
```

## 安全设计

| 措施 | 原因 |
|---|---|
| sudoers 窄授权 | `/etc/sudoers.d/announce-web` 只放行 3 条命令：`systemctl restart announce-rs`、`announce-schedule-apply`（排班应用）、`announce-net-apply.sh`（网络配置，脚本内自带 `valid_host` 严格参数校验）。完整片段见 `deploy/sudoers.announce-web`（2026-10-23 修复：net-apply 一直未授权导致网络页必失败）。Web 进程即使被攻破也只能执行这三个白名单动作 |
| `validate_rtsp` 纵深防御 | 非法 RTSP 源在入口和 ffmpeg 调用前各拦一道（防内网 SSRF 类探测） |
| 请求体上限 | 全部 POST 端点经 `_read_body`：负/非法 Content-Length 400，超 64KB 413 并断开连接（防 `read(-1)` 挂线程与内存打爆） |
| 同源校验 | POST 入口校验 Origin/Referer 与 Host 一致（防跨站页面触发改配置/断网） |
| 乐观锁 | `/api/config` 下发内容指纹 `rev`，保存时校验，两窗口并发保存后者被 409 拦下 |
| 配置原子写 | tmp + `os.replace`，主服务/配置器永不读到半截 config.json |
| `CONFIG_LOCK` 串行化 | 多浏览器同时保存不互相踩 |
| 无认证但限内网 | 调试台定位；`NET_DEFAULT_IP=192.168.31.1` 是直连调试出厂值，有路由器现场勿用（多为网关） |

## 本地验证记录（2026-10-07）

| 项 | 结果 |
|---|---|
| 首页渲染 | ✅ "叫号播报 · 配置台"，控件齐全，无 JS 报错 |
| `/api/config` `/api/status` `/api/devices` `/api/audio` `/api/logs` | ✅ 全部正常 |
| `/api/snapshot`(simulate) | ✅ 1920×1080 JPEG；**需 sim_frame.jpg 与脚本同目录** |
| `/api/snapshot`(USB/默认) | ✅ 修复后正常：主程序在跑时走 `_live/req` 握手拿实时帧（960×540 降采样），没跑才自己开 ffmpeg |
| `/api/test_ocr` | ✅ 修复后 ok=true；ROI 按实际帧尺寸自动换算裁剪 |
| 实时预览 | 页面默认关闭（设计如此，看画面用「抓帧预览」按钮） |
| 保存 | 本地无 systemd 单元且无免密 sudo，**会提示"重启失败: sudo: a password is required"**——配置已写入，属预期；手动重启 announce-rs 即可生效（生产机有窄授权 sudoers，此步自动完成） |

### 试跑发现并修复的 3 个 bug（均在 config_web.py，附根因）

1. **USB 抓帧误判主服务状态**：原逻辑用 `systemctl show` 判断服务是否在跑，
   手工 nohup 拉起的 announce-rs 被判为 inactive → 自己开 ffmpeg → 撞上
   "Device or resource busy"。改为 `announce_running()` 用 pgrep 探测进程，
   systemd/手工拉起都能识别。
2. **不认 `/dev/videoN` 源写法**：Rust 版 cfg 允许 `camera_source: "/dev/video0"`，
   配置台只认数字索引，页面默认抓帧(用配置里的源)直接报"非法源"。
   新增 `norm_src()` 归一化 `/dev/videoN` → `N`。
3. **test_ocr 裁剪越界**：服务握手返回的预览帧是降采样的(960×540)，ROI 坐标
   是采集分辨率尺度(1920×1080)，直接 crop 报 "Invalid too big"。现在用
   ffprobe 探测实际帧尺寸，按比例换算 ROI 并收敛到帧内。

## 与 Rust 主服务的分工

```
config_web.py(调试时运行)            announce-rs(常驻服务)
─────────────────────────           ─────────────────────
改 config.json ──原子写──▶ ────────▶ 下次重启读到新配置
sudo systemctl restart ────────────▶ 重启加载
test_ocr/test_tts ──独立进程试跑──▶ 不影响主服务
```

主服务本身不内置 HTTP 界面（保持零依赖+低占用）；调试台按需启停，
不跑时对生产零影响。`spell_tokens()`（逐字读法）与 Rust 版逻辑对齐，
试听结果与真实播报一致。
