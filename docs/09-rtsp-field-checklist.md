# 09 · 现场 RTSP 摄像头接入操作清单

适用：debian 部署机 + RTSP 网络摄像头首次接入/换机重接。
按顺序执行，每步有验证点；常见故障见文末速查表。

## 0. 出发前确认（办公室完成）

- [ ] 笔记本能 SSH debian（debian@主机IP，与现场交换机同段）
- [ ] 摄像头管理密码、默认 RTSP 地址已查（品牌手册/后台）
- [ ] `systemctl is-enabled announce-rs announce-piper announce-web` 全 enabled

## 1. 物理接入与连通性

1. 摄像头上电、网线接入与 debian 同网段的交换机
2. debian 上 `ping <摄像头IP>` 通
3. debian 上验证流（**必过验证点**）：

```bash
ffprobe -rtsp_transport tcp -i "rtsp://用户:密码@摄像头IP:554/stream1" 2>&1 | grep -m1 Video
```

能列出 `Video: h264` 即通。失败对照速查表（文末）。

## 2. 选码流

- [ ] 后台把**子码流**设为 640×360（叫号屏 OCR 足够，带宽/解码最省）
- [ ] 记下实际分辨率，第 3 步要用

## 3. 改配置（两种方式选一）

**方式 A · Web 台（推荐）**：浏览器开 `http://<debian-ip>:8080`
**方式 B · 直接编辑**：`/home/debian/anntest/config.json`

| 字段 | 填什么 |
|---|---|
| `camera_source` | `rtsp://用户:密码@IP:554/stream1`（原值 `0`） |
| `capture_width` / `capture_height` | 与子码流一致（如 640 / 360） |
| `roi` | 先按比例粗填，第 5 步精标 |

## 4. 启动服务

工作时间调度会自动拉起；现场调试手动启动：

```bash
echo root | su -c 'systemctl start announce-worktime-start.service' root
systemctl is-active announce-rs announce-piper announce-web   # 三行 active
```

> debian 只开放窄授权 sudoers，全量启动需 su；`sudo systemctl restart announce-rs`
> 免密可用（Web 台"保存并重启"走的就是它）。

## 5. 标定 ROI（关键步骤）

1. Web 台 → **抓帧预览**，确认画面清晰、无过曝
2. 拖框，**只框"当前办理"两行**：
   - 排除下方滚动字幕区（否则持续误触发 OCR）
   - 排除玻璃反光区（换个安装角度躲开）
   - 排除旁边的事项清单屏
3. **保存** → announce-rs 自动重启生效
4. 验证：`ls -lt /home/debian/anntest/_live/ | head` 应持续出现新触发帧

## 6. 功能验证

```bash
journalctl -u announce-rs -f          # 盯日志
```

- [ ] 屏幕换号 → `[播报] 请GAxxx到Cxx号窗口办理`，全程 ≤3s
- [ ] 现场音箱/喇叭出声（音量 `alsamixer`，TTS=piper 中文女声，读号逐字）
- [ ] `announce_log.csv` 出现新 OK 行（Excel 可开，带 BOM）
- [ ] 同号不重复播报；程序重启后首播一次属正常（去重集清空）

## 7. 收工前回归

- [ ] `systemctl list-timers | grep announce`：cleanup 03:30 / start 08:00 / stop 18:00 都在
- [ ] 若非工作时间调试完：`su -c 'systemctl start announce-worktime-stop.service' root`
      停回调度态，明天 08:00 自动上岗
- [ ] 摄像头断电 10s 再上电 → 日志出现断流重拉并自动恢复（自愈验证，可选）

## 故障速查

| 现象 | 处置 |
|---|---|
| ffprobe 超时 | 网段/防火墙：`ping` 通再查 554 端口 `nc -zv IP 554` |
| ffprobe 401 | 账号密码错；密码含 `@#&` 需 URL 编码 |
| ffprobe 461 | 摄像头限制传输协议，后台放开 TCP interleaved |
| 日志刷 `取帧失败` | 流断了：重跑第 1 步验证；3s 自愈机制会持续重试 |
| `[警告] ROI 超出帧` | 分辨率与 `capture_width/height` 不一致，重做第 3 步 |
| 有识别无播报 | `journalctl -u announce-piper`：模型加载失败则重启该服务；音频走 05-tts.md |
| Web 台抓帧黑图 | 主服务占用/未出流：确认 announce-rs active 且流可达 |

## 验收标准（全部满足才收工）

1. ffprobe TCP 拉流通
2. ROI 标定完成且 `_live/` 存证帧框内无杂物
3. 真实换号 ≤3s 出声播报，日志 OK 行落盘
4. 3 个 timer 计划正常，服务回到调度状态
