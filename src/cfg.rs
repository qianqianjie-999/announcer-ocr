// cfg.rs — 配置结构（与 Python 版 config.json 完全同 schema）
use serde::Deserialize;

fn d25() -> f64 { 25.0 }
fn d0015() -> f64 { 0.0015 }
fn d2() -> u32 { 2 }
fn d1() -> u32 { 1 }
fn d40() -> f64 { 4.0 }
fn d30() -> f64 { 3.0 }
fn d05() -> f64 { 0.5 }
fn d5() -> f64 { 5.0 }
fn f30() -> f64 { 3.0 }
fn d10s() -> f64 { 10.0 }
fn def_true() -> bool { true }
fn p51888() -> u16 { 51888 }
fn logf() -> String { "announce_log.csv".into() }
fn cw() -> u32 { 1920 }
fn chh() -> u32 { 1080 }
fn tbin() -> String { "tesseract".into() }
fn tlang() -> String { "chi_sim".into() }
fn tpsm() -> u32 { 11 }
fn tdpi() -> u32 { 150 }
fn tenabled() -> bool { true }
fn tengine() -> String { "auto".into() }
fn trepeat() -> u32 { 2 }
fn trate() -> i32 { 135 }
fn ttemplate() -> String { "请{number}到{window}号窗口办理".into() }
fn tgap() -> f64 { 0.35 }
fn dhealth() -> f64 { 60.0 }
fn dstale() -> f64 { 60.0 }
fn logmb() -> f64 { 8.0 }
fn logkeep() -> u32 { 6 }
fn logarch() -> String { "logs_archive".into() }

#[derive(Deserialize, Clone, Default)]
pub struct Detect {
    #[serde(default = "d25")] pub diff_threshold: f64,
    #[serde(default = "d0015")] pub changed_ratio: f64,
    #[serde(default = "d2")] pub stable_frames: u32,
    #[serde(default = "d40")] pub confirm_ttl_sec: f64,
    #[serde(default = "d30")] pub active_sec: f64,
    #[serde(default = "d05")] pub min_interval_sec: f64,
    #[serde(default = "d30")] pub fallback_ocr_sec: f64,
    /// 静态兜底 OCR 间隔(秒)。屏面静止且上一轮已读出内容时, 反复重读同一批像素
    /// 纯烧 CPU(实测占满一核), 还让"变化发生时正卡在 OCR 里"的概率逼近 100%。
    /// 此场景下兜底间隔从 fallback_ocr_sec(3s) 放宽到本值(默认 10s)。
    #[serde(default = "d10s")] pub static_fb_sec: f64,
    /// 稳帧再认: 帧差判"有变化"后, 先静置本秒数再 OCR。
    /// 叫号屏换号带 0.3~1s 的滚动/淡入动画, 动画中间帧 OCR 会读出错号
    /// (现场实测过渡帧把 GA174 读成 GAT174 -> 播报 AT174)。
    /// 设 0(或远小于帧间隔的值)即几乎不静置, 退回旧行为。
    /// 另注: 变化持续超过 SETTLE_MAX_WAIT_SECS(2s)会强制认一次, 防"画面一直在变
    /// 导致永不 OCR"的静默失效。
    #[serde(default = "d05")] pub settle_sec: f64,
    /// 静帧快速确认轮数。候选出现过一次 + 画面连续 N 轮无变化 -> 直接确认播报,
    /// 免掉第二轮 OCR(一轮 1.6~4.8s, 是 5~10s 端到端延迟的最大单项)。
    /// 设 0 可关闭快速确认(完全退回"两轮 OCR 投票"的旧行为)。默认 2。
    #[serde(default = "d2")] pub static_confirm_ticks: u32,
    /// 号码闸门: 跳幅分档的「第一档边界」。默认 2。
    ///
    /// 由来(2026-10-21 现场): 屏幕改成三行列表后, 单数字误读会读出"看起来完全
    /// 合理"的号 —— GA158 被读成 GA198(5→9)、GA150 被读成 GA190(0→9), 都直接
    /// 播了 3 遍, 而且误读号会占住当日去重表, 导致真号来了反而不再播报。
    /// 现场编号严格递增, 所以"跳幅闸门"能直接拦掉这类误读。
    ///
    /// 分档(详见 main.rs 的 `jump_extra_confirm`): 跳幅 ≤3 立即播; ≤本值多要 2 个
    /// 确认量; 再大到 +30 多要 4 个; >30 直接挡(20s 静默后兜底放行)。
    /// 设 0 = 关闭闸门(退回旧行为: 读到什么播什么)。
    #[serde(default = "d2")] pub max_jump: u32,
    /// 叫号列表的识别行数(1 = 只读 ROI 那一行)。
    /// 屏幕是"多行列表"且最新号不总在最上面时设为 3, 配合 row_pitch 用。
    #[serde(default = "d1")] pub rows: u32,
    /// 行距(像素): 相邻两行文字框的纵向间距。0 = 不分行(退化为单行)。
    ///
    /// ★这是"缩放后画布"上的绝对像素(见 capture 的 scale=)。**换码流/换分辨率
    /// 必须重新标定**, 否则第 2/3 行会切到行间空隙(2026-10-22 现场: 辅码流
    /// 画布 704×576 下的 73 被按比例放大成 137, 而真实行距只有 70 → 第 2 行被
    /// 跳过、第 3 行切到空白, 现象是"看着识别了却不播报")。
    /// 推荐改用 `row_pitch_ratio` —— 相对行高表达, 换分辨率自动跟随。
    #[serde(default)] pub row_pitch: u32,
    /// 行距的**相对**表达: 相邻两行间距 = roi 高度 × 本值。
    ///
    /// 实测行距≈行高(主码流 1920×1080: 行高 69、行距 70), 所以常用 **1.0**。
    /// 仅在 `row_pitch = 0`(未给绝对像素)时生效; 两者都给则以 `row_pitch` 为准。
    /// 默认 0 = 不使用(保持"row_pitch=0 即单行"的旧语义)。
    #[serde(default)] pub row_pitch_ratio: f64,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}
impl Default for OneOrMany {
    fn default() -> Self { OneOrMany::Many(vec![]) }
}

#[derive(Deserialize, Clone, Default)]
pub struct OcrCfg {
    #[serde(default = "tbin")] pub bin: String,
    #[serde(default = "tlang")] pub lang: String,
    #[serde(default = "tpsm")] pub psm: u32,
    #[serde(default = "tdpi")] pub dpi: u32,
    #[serde(default)] pub pattern: Option<OneOrMany>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Tts {
    #[serde(default = "tenabled")] pub enabled: bool,
    #[serde(default = "tengine")] pub engine: String,
    #[serde(default = "trepeat")] pub repeat: u32,
    #[serde(default = "trate")] pub rate: i32,
    #[serde(default = "ttemplate")] pub template: String,
    #[serde(default)] pub spell_number: bool,
    #[serde(default = "tgap")] pub gap_sec: f64,
    #[serde(default)] pub aplay_device: Option<String>,
    #[serde(default)] pub piper_bin: Option<String>,
    #[serde(default)] pub piper_model: Option<String>,
    #[serde(default)] pub piper_lib_dir: Option<String>,
    /// piper 语速缩放(length_scale), 越大越慢, 默认 1.0。稍慢推荐 1.2。
    #[serde(default)] pub piper_length_scale: Option<f64>,
    /// 常驻合成服务地址(如 http://127.0.0.1:5001/)。设了就走服务(约2~3秒出声),
    /// 服务不可用时自动回退 CLI 直调(每次重载模型约13秒)。为空则只用 CLI。
    #[serde(default)] pub piper_url: Option<String>,
    /// 播报窗口号时是否念字母前缀。true: C04 → "西 零 四"; false/缺省: 只念数字 "零 四"。
    /// 现场要求不念 C(听感突兀), 故默认关; 想恢复念字母在 config.json 置 true 即可。
    #[serde(default)] pub speak_window_letter: Option<bool>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default)] pub camera_source: serde_json::Value,
    #[serde(default)] pub roi: Option<Vec<f64>>,
    #[serde(default = "f30")] pub fps: f64,
    #[serde(default = "p51888")] pub lock_port: u16,
    #[serde(default = "def_true")] pub diag_heartbeat: bool,
    #[serde(default = "d5")] pub diag_every_sec: f64,
    #[serde(default = "def_true")] pub save_trigger_frames: bool,
    /// 「健康心跳」阈值(分钟): 距上次成功播报超过这么久就打印一条 `[健康]` 告警,
    /// 用于发现"进程活着、链路已死"的静默软故障 —— 典型是摄像头黑屏/失焦后帧差与
    /// OCR 双双归零, 程序照常空转一整天却一声不响(评审 P2-7)。
    /// 60 = 默认(1 小时); **0 = 关闭**。若现场本就有长时间无人叫号的时段(午休/夜班),
    /// 告警会如实报"很久没播报" —— 那属预期而非故障, 按作息调大或设为 0 即可。
    #[serde(default = "dhealth")] pub health_alert_min: f64,
    #[serde(default = "logf")] pub log_file: String,
    /// 已播报去重表(`announced`)的过期策略, 单位秒。
    /// **0(默认)= 每日本地零点清零** —— 与"叫号号码按天复用"的业务语义一致;
    /// 正数 = 滚动 TTL, 距最后一次播报超过 N 秒即允许重播;
    /// 负数 = 永不清理(旧行为, 仅供回归对照; 仍有 4096 条硬上限兜底)。
    /// 为什么要有这个开关: 旧实现只增不删, 号码跨天复用时会被永久判为"已播报"而
    /// 静默哑掉; 现场没暴露只是因为 worktime 定时器每天 18:00 重启顺带清了一次。
    #[serde(default)] pub announced_ttl_sec: f64,
    /// 播报日志按大小轮转的阈值(MB)。0 = 关闭, 完全交给 deploy/announce-cleanup.sh。
    /// 默认 8MB 与清理脚本的阈值一致; 归档命名也一致, 两边可共存不冲突。
    #[serde(default = "logmb")] pub log_max_mb: f64,
    /// 日志归档保留个数(不含当前文件), 与清理脚本的 ARCHIVE_KEEP 保持一致。
    #[serde(default = "logkeep")] pub log_keep: u32,
    /// 日志归档目录。只在 log_max_mb > 0 时使用。
    #[serde(default = "logarch")] pub log_archive_dir: String,
    #[serde(default = "cw")] pub capture_width: u32,
    #[serde(default = "chh")] pub capture_height: u32,
    /// 无帧看门狗阈值(秒): 连续这么久没读到新帧就 kill 掉 ffmpeg 触发重拉。
    /// 针对的是 **RTSP 半开** —— TCP 未断、ffmpeg 进程健在、但不再吐帧, 此时
    /// `read_exact` 会永远阻塞(既不 Ok 也不 Err), 现有的"管道断开才重拉"逻辑
    /// 永远等不到触发条件: 现象就是进程活着、日志无错、但一声不播。
    /// 60 = 默认(摄像头 15fps 时正常帧间隔约 0.07s, 留足余量); **0 = 关闭**。
    #[serde(default = "dstale")] pub frame_stale_sec: f64,
    #[serde(default)] pub detect: Detect,
    #[serde(default)] pub ocr: OcrCfg,
    #[serde(default)] pub tts: Tts,
    /// 作息闸(2026-10-22 中危修复): config.json 里一直有 schedule 键(Web 台
    /// 保存的工作时段), 但 Rust 侧从未读取 —— 服务开机自启(WantedBy=
    /// multi-user.target)后夜里重启主机就会整夜播报。现在主程序自己把关:
    /// enabled 且当前不在 work_start~work_end 内就不播报(OCR 链照常, 恢复
    /// 后自动续播)。与 systemd worktime timer 互为兜底: timer 漏停(如补跑
    /// 竞态)时程序侧仍静音; 配置 enabled=false 时闸不生效, 行为同旧版。
    #[serde(default)] pub schedule: Schedule,
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq)]
pub struct Schedule {
    #[serde(default)]
    pub enabled: bool,
    /// 工作开始 "HH:MM"。与 end 相等视为全天(闸不生效)。
    #[serde(default = "work_start_d")]
    pub work_start: String,
    /// 工作结束 "HH:MM"。start > end 表示跨零点(如 22:00~06:00 夜班)。
    #[serde(default = "work_end_d")]
    pub work_end: String,
}
fn work_start_d() -> String { "08:00".into() }
fn work_end_d() -> String { "18:00".into() }

impl Config {
    pub fn load(path: &str) -> Result<Config, String> {
        let txt = std::fs::read_to_string(path)
            .map_err(|e| format!("读不到配置 {}: {}", path, e))?;
        let cfg: Config =
            serde_json::from_str(&txt).map_err(|e| format!("配置解析失败: {}", e))?;
        cfg.validate().map(|_| cfg)
    }

    /// 启动前语义校验 —— 抓"serde 类型合法但运行期致命"的配置。
    /// ★capture_width/height ≥ 1(2026-10-22 第三轮审查): 0 会合法通过 serde(u32),
    ///   capture.rs 按它分配 frame 缓冲, 0 字节缓冲上 read_exact 恒 Ok -> 抓帧线程
    ///   100% CPU 死循环且每帧刷"[预览失败]"。宁可拒启, 不带病运行。
    pub fn validate(&self) -> Result<(), String> {
        if self.capture_width == 0 || self.capture_height == 0 {
            return Err(format!(
                "capture_width/height 不能为 0 (当前 {}x{}): 0 尺寸会让抓帧线程死循环",
                self.capture_width, self.capture_height
            ));
        }
        Ok(())
    }

    /// camera_source: 数字(USB索引) / "simulate" / "rtsp://..." / "/dev/videoN"
    pub fn source(&self) -> SourceKind {
        match &self.camera_source {
            serde_json::Value::String(s) => {
                let s = s.trim();
                if s.eq_ignore_ascii_case("simulate") {
                    SourceKind::Simulate
                } else if s.starts_with("/dev/video") {
                    SourceKind::Device(s.to_string())
                } else if s.starts_with("rtsp://") || s.starts_with("http://")
                    || s.starts_with("https://") {
                    SourceKind::Net(s.to_string())
                } else if let Ok(n) = s.parse::<u32>() {
                    SourceKind::Device(format!("/dev/video{}", n))
                } else {
                    SourceKind::Device(s.to_string())
                }
            }
            serde_json::Value::Number(n) => {
                SourceKind::Device(format!("/dev/video{}", n.as_i64().unwrap_or(0)))
            }
            _ => SourceKind::Device("/dev/video0".into()),
        }
    }
}

impl OcrCfg {
    pub fn patterns(&self) -> Vec<String> {
        match &self.pattern {
            Some(OneOrMany::One(s)) => vec![s.clone()],
            Some(OneOrMany::Many(v)) => v.clone(),
            None => vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    Simulate,
    Net(String),
    Device(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写临时配置文件(项目不带 tempfile 依赖, 用 pid+原子计数保证并发唯一)。
    fn write_tmp_cfg(json: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "anntest_cfg_{}_{}.json",
            std::process::id(),
            n
        ));
        std::fs::write(&p, json).expect("写临时配置失败");
        p.to_string_lossy().into_owned()
    }

    /// Config 未 derive Debug(无需为测试加派生), 用 match 解包 Err。
    fn expect_load_err(json: &str) -> String {
        match Config::load(&write_tmp_cfg(json)) {
            Err(e) => e,
            Ok(_) => panic!("配置应被 validate 拒绝: {json}"),
        }
    }

    /// 核心回归(2026-10-22 第三轮审查): capture_width=0 会被 serde(u32) 合法接受,
    /// 旧版 Config::load 照单全收 -> capture.rs 分配 0 字节缓冲 -> 抓帧线程
    /// 100% CPU 死循环。修复后必须在启动时拒绝。
    #[test]
    fn load_rejects_zero_width() {
        let err = expect_load_err(r#"{"capture_width": 0}"#);
        assert!(err.contains("不能为 0"), "错误信息应可读: {err}");
    }

    #[test]
    fn load_rejects_zero_height() {
        let err = expect_load_err(r#"{"capture_height": 0}"#);
        assert!(err.contains("不能为 0"), "错误信息应可读: {err}");
    }

    #[test]
    fn load_accepts_normal_size_and_empty_json() {
        // 显式正常尺寸: 必须原样通过
        let p = write_tmp_cfg(r#"{"capture_width": 1280, "capture_height": 720}"#);
        let cfg = match Config::load(&p) {
            Ok(c) => c,
            Err(e) => panic!("正常尺寸不应被拒: {e}"),
        };
        assert_eq!((cfg.capture_width, cfg.capture_height), (1280, 720));
        std::fs::remove_file(&p).ok();
        // 全默认(空对象): 默认 1920x1080, 也必须放行 —— 防止校验误伤缺省路径
        let p = write_tmp_cfg("{}");
        let cfg = match Config::load(&p) {
            Ok(c) => c,
            Err(e) => panic!("默认配置不应被拒: {e}"),
        };
        assert!(cfg.capture_width > 0 && cfg.capture_height > 0);
        std::fs::remove_file(&p).ok();
    }
}
