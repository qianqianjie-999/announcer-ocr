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
    /// 停稳闸: 帧差变化后, 连续 N 轮无变化(画面停稳)才允许跑 OCR。
    /// 动画/换卡进行中 OCR 结果必然作废, 却把单次 OCR 从 1.6s 拖到 4.6s
    /// (CPU 被帧差+OCR+增强兜底链抢爆), 是真实换卡 6.9s 的主因。
    /// 停稳后 streak 必然 >= static_confirm_ticks, 静帧快速确认自动接管。
    /// 设 0 可关闭(旧行为, 变化立即 OCR)。默认 3(fps=5 时约 0.6s)。
    #[serde(default = "d3u")] pub settle_ticks: u32,
}

fn d3u() -> u32 { 3 }

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
    /// 常驻合成服务地址(如 http://127.0.0.1:5001/)。设了就走服务(约2~3秒出声),
    /// 服务不可用时自动回退 CLI 直调(每次重载模型约13秒)。为空则只用 CLI。
    #[serde(default)] pub piper_url: Option<String>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default)] pub camera_source: serde_json::Value,
    #[serde(default)] pub roi: Option<Vec<f64>>,
    #[serde(default = "f30")] pub fps: f64,
    #[serde(default = "def_true")] pub low_latency: bool,
    #[serde(default = "p51888")] pub lock_port: u16,
    #[serde(default = "def_true")] pub diag_heartbeat: bool,
    #[serde(default = "d5")] pub diag_every_sec: f64,
    #[serde(default = "def_true")] pub save_trigger_frames: bool,
    #[serde(default = "logf")] pub log_file: String,
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
