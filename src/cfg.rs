// cfg.rs — 配置结构（与 Python 版 config.json 完全同 schema）
use serde::Deserialize;

fn d25() -> f64 { 25.0 }
fn d0015() -> f64 { 0.0015 }
fn d2() -> u32 { 2 }
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
    /// 静帧快速确认轮数。候选出现过一次 + 画面连续 N 轮无变化 -> 直接确认播报,
    /// 免掉第二轮 OCR(一轮 1.6~4.8s, 是 5~10s 端到端延迟的最大单项)。
    /// 设 0 可关闭快速确认(完全退回"两轮 OCR 投票"的旧行为)。默认 2。
    #[serde(default = "d2")] pub static_confirm_ticks: u32,
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
    #[serde(default)] pub detect: Detect,
    #[serde(default)] pub ocr: OcrCfg,
    #[serde(default)] pub tts: Tts,
}

impl Config {
    pub fn load(path: &str) -> Result<Config, String> {
        let txt = std::fs::read_to_string(path)
            .map_err(|e| format!("读不到配置 {}: {}", path, e))?;
        serde_json::from_str(&txt).map_err(|e| format!("配置解析失败: {}", e))
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
