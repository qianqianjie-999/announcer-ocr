// tts.rs — 语音合成与播放（对齐 announce.py make_tts_fn / play / spell_tokens）
// 链路: espeak-ng|piper 先合成 wav -> aplay 播放。
// 为什么不直接让 espeak 自己出声: espeak 直连会与 pipewire/pulse 抢声卡,
// 先落 wav 再 aplay 可用 ALSA 设备回退列表解决占用问题(与 Python 版一致)。
use crate::cfg::Tts;
use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub trait Engine: Send {
    fn name(&self) -> &'static str;
    /// 一次调用完整播放 times 遍。
    fn speak(&self, text: &str, times: u32) -> Result<(), String>;
}

/// 带超时的子进程执行(收集 stdout/stderr)。
/// 超时意义: aplay/espeak 一旦挂死会卡死唯一的播报线程, 必须能强制跳过。
fn run_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // spawn 前已设 Stdio::piped(), 正常不会为 None; 兜底路径必须顺手收尸,
    // 否则半途返回 Err 会把子进程留成孤儿(Child 析构不 kill)。
    let (mut so, mut se) = match (child.stdout.take(), child.stderr.take()) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("子进程管道不可用".to_string());
        }
    };
    // 读线程防管道写满后子进程被阻塞
    let (tx1, rx1) = mpsc::channel();
    let (tx2, rx2) = mpsc::channel();
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        let _ = tx1.send(v);
    });
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        let _ = tx2.send(v);
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = rx1.recv().unwrap_or_default();
                let stderr = rx2.recv().unwrap_or_default();
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("命令超时({}s)已终止", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 简易 which: 遍历 PATH 找可执行文件。
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

/// 播放 wav: 优先配置的 ALSA 设备, 被占用时自动退回 default。
/// Linux 上有 pipewire/pulse 时会独占声卡, 直连 plughw 会 Device busy。
fn play(wav: &str, times: u32, dev_pref: Option<&str>, gap: f64) -> Result<(), String> {
    let mut devs: Vec<Option<String>> = Vec::new();
    if let Some(d) = dev_pref {
        devs.push(Some(d.to_string()));
    }
    devs.push(None); // 不带 -D, 用系统默认
    devs.push(Some("default".into()));
    let mut last = String::new();
    for dev in &devs {
        let mut all_ok = true;
        for _ in 0..times {
            let mut c = Command::new("aplay");
            c.arg("-q");
            if let Some(d) = dev {
                c.arg("-D").arg(d);
            }
            c.arg(wav);
            match run_timeout(&mut c, Duration::from_secs(120)) {
                Ok(o) if o.status.success() => {
                    std::thread::sleep(Duration::from_secs_f64(gap));
                }
                Ok(o) => {
                    last = format!(
                        "aplay 退出 {:?}: {}",
                        o.status.code(),
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                    all_ok = false;
                    break;
                }
                Err(e) => {
                    last = e;
                    all_ok = false;
                    break;
                }
            }
        }
        if all_ok {
            return Ok(());
        }
    }
    Err(format!("aplay 播放失败(已尝试 {:?}): {}", devs.len(), last))
}

// ---------------------------------------------------------------- piper
/// 向常驻合成服务 POST 文本, 返回 wav 字节。
/// 手写最小 HTTP/1.1 而非引入 reqwest: 只走本地回环, 省下十几 MB 二进制与一堆依赖。
/// pub(crate): 除正式播报外, announcer 的预热(prefetch)也用它把合成赶在确认期之前。
pub(crate) fn http_post_wav(url: &str, text: &str) -> Result<Vec<u8>, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "只支持 http:// 地址".to_string())?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = hostport
        .to_socket_addrs()
        .map_err(|e| format!("地址解析失败: {}", e))?
        .next()
        .ok_or_else(|| "地址为空".to_string())?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("连接失败: {}", e))?;
    s.set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| format!("设置超时失败: {}", e))?;
    let body = text.as_bytes();
    let head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        path,
        hostport,
        body.len()
    );
    s.write_all(head.as_bytes())
        .and_then(|_| s.write_all(body))
        .map_err(|e| format!("发送请求失败: {}", e))?;
    let mut buf = Vec::with_capacity(1 << 18);
    s.read_to_end(&mut buf)
        .map_err(|e| format!("读取响应失败: {}", e))?;
    let pos = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "响应格式异常".to_string())?;
    let status_line = String::from_utf8_lossy(&buf[..pos])
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    if !status_line.contains(" 200") {
        return Err(format!("服务返回 {}", status_line));
    }
    Ok(buf[pos + 4..].to_vec())
}

struct Piper {
    bin: String,
    model: String,
    lib_dir: String,
    gap: f64,
    dev: Option<String>,
    /// 语速缩放(length_scale): 越大越慢, None 用模型默认(1.0)
    length_scale: Option<f64>,
    /// 常驻服务地址(可选): 命中则不启新进程, 出声快 5~10 倍
    server_url: Option<String>,
}

impl Engine for Piper {
    fn name(&self) -> &'static str {
        "piper"
    }
    fn speak(&self, text: &str, times: u32) -> Result<(), String> {
        let wav = std::env::temp_dir().join("tts_piper.wav");
        if let Some(url) = self.server_url.as_deref().filter(|u| !u.is_empty()) {
            match http_post_wav(url, text) {
                Ok(bytes) if !bytes.is_empty() => {
                    std::fs::write(&wav, &bytes)
                        .map_err(|e| format!("写 wav 失败: {}", e))?;
                    return play(
                        wav.to_str().unwrap_or("tts_piper.wav"),
                        times,
                        self.dev.as_deref(),
                        self.gap,
                    );
                }
                Ok(_) => eprintln!("[TTS] 常驻服务返回空音频, 回退 CLI 合成"),
                Err(e) => eprintln!("[TTS] 常驻服务不可用({}), 回退 CLI 合成", e),
            }
        }
        // 走到这里说明服务不可用, 只能靠 CLI; 二者都没有就别再启动空进程
        if self.bin.is_empty() || self.model.is_empty() {
            return Err("piper 常驻服务不可用, 且 CLI 未配置".into());
        }
        // piper 需要同目录下的动态库与 espeak-ng-data, 必须显式给出
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--model")
            .arg(&self.model)
            .arg("--output_file")
            .arg(&wav)
            .stdin(Stdio::piped());
        // 语速缩放: 越大越慢(稍慢推荐 1.2); 不配置则用模型默认
        if let Some(ls) = self.length_scale {
            cmd.arg("--length-scale").arg(ls.to_string());
        }
        cmd.env(
            "LD_LIBRARY_PATH",
            format!(
                "{}:{}",
                self.lib_dir,
                std::env::var("LD_LIBRARY_PATH").unwrap_or_default()
            ),
        );
        // ESPEAK_DATA_PATH 只在 lib_dir 下确实有 espeak-ng-data 时设置:
        // pip 版 piper 的语音数据打包在 piper_phonemize 内部, 指错路径反而会破坏音素化。
        if std::path::Path::new(&self.lib_dir).join("espeak-ng-data").exists() {
            cmd.env("ESPEAK_DATA_PATH", &self.lib_dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("piper 启动失败: {}", e))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(text.as_bytes());
        }
        // stdin 已 drop(关闭), piper 收到 EOF 后开始合成; 轮询等待, 带超时防挂死
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(180) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("piper 超时(180s)已终止".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("piper 等待失败: {}", e)),
            }
        };
        if !status.success() {
            return Err(format!("piper 退出码 {:?}", status.code()));
        }
        play(wav.to_str().unwrap_or("tts_piper.wav"), times, self.dev.as_deref(), self.gap)
    }
}

// ---------------------------------------------------------------- espeak
struct Espeak {
    exe: String,
    voice: String, // 中文音色名: 新版 espeak-ng 用 cmn, 老版用 zh
    speed: u32,    // espeak 用 wpm, 取慢速
    gap: f64,
    dev: Option<String>,
}

impl Engine for Espeak {
    fn name(&self) -> &'static str {
        "espeak"
    }
    fn speak(&self, text: &str, times: u32) -> Result<(), String> {
        let wav = std::env::temp_dir().join("tts_espeak.wav");
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-v").arg(&self.voice)
            .arg("-s").arg(self.speed.to_string())
            .arg("-w").arg(&wav)
            .arg(text);
        run_timeout(&mut cmd, Duration::from_secs(120))
            .map_err(|e| format!("espeak 失败: {}", e))?;
        play(wav.to_str().unwrap_or("tts_espeak.wav"), times, self.dev.as_deref(), self.gap)
    }
}

/// 探测可用的中文音色: 优先 cmn(espeak-ng 1.5x 的普通话),
/// 没有则退回 zh(老版别名)。写死 zh 在老版可用、新版虽能别名解析但语义不明确。
fn espeak_voice(exe: &str) -> String {
    let out = Command::new(exe)
        .args(["--voices=cmn"])
        .output();
    match out {
        Ok(o) => {
            let txt = String::from_utf8_lossy(&o.stdout);
            if txt.lines().any(|l| l.split_whitespace().nth(1) == Some("cmn")) {
                "cmn".into()
            } else {
                "zh".into()
            }
        }
        Err(_) => "zh".into(),
    }
}

fn try_piper(cfg: &Tts) -> Option<Box<dyn Engine>> {
    let url = cfg.piper_url.clone().filter(|u| !u.is_empty());
    // bin/model 只影响 CLI 回退路径; 常驻服务在线时二者可为空
    let bin = cfg
        .piper_bin
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| which("piper"))
        .filter(|b| std::path::Path::new(b).exists());
    let model = cfg
        .piper_model
        .clone()
        .filter(|m| !m.is_empty())
        .filter(|m| std::path::Path::new(m).exists());
    if bin.is_none() && model.is_none() && url.is_none() {
        return None;
    }
    let lib_dir = cfg
        .piper_lib_dir
        .clone()
        .unwrap_or_else(|| {
            bin.as_deref()
                .and_then(|b| std::path::Path::new(b).parent())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    Some(Box::new(Piper {
        bin: bin.unwrap_or_default(),
        model: model.unwrap_or_default(),
        lib_dir,
        gap: cfg.gap_sec,
        dev: cfg.aplay_device.clone(),
        length_scale: cfg.piper_length_scale,
        server_url: url,
    }))
}

fn try_espeak(cfg: &Tts) -> Option<Box<dyn Engine>> {
    let exe = which("espeak-ng").or_else(|| which("espeak"))?;
    // espeak 用 wpm, 取慢速(与 Python 版 max(80, rate*0.7) 一致)
    let speed = std::cmp::max(80, (cfg.rate as f64 * 0.7) as u32);
    let voice = espeak_voice(&exe);
    Some(Box::new(Espeak { exe, voice, speed, gap: cfg.gap_sec, dev: cfg.aplay_device.clone() }))
}

/// 按 config.tts.engine 选择发声引擎, auto 时按 piper > espeak 探测(目标平台 Linux;
/// Python 版的 windows/sapi 分支在靶机不存在, 不复刻)。
pub fn make_tts(cfg: &Tts) -> Result<Box<dyn Engine>, String> {
    let order: Vec<&str> = match cfg.engine.as_str() {
        "piper" => vec!["piper"],
        "espeak" => vec!["espeak"],
        _ => vec!["piper", "espeak"],
    };
    for e in order {
        let built = match e {
            "piper" => try_piper(cfg),
            _ => try_espeak(cfg),
        };
        if let Some(engine) = built {
            println!("[TTS] 使用引擎: {}", engine.name());
            return Ok(engine);
        }
    }
    Err("未找到可用TTS: 请安装 espeak-ng 或 piper-tts".into())
}

// ---------------------------------------------------------------- 播报文本
const DIGIT_CN: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

/// 号码/窗口号逐字符展开: GA204 -> G A 二 零 四, C06 -> C 零 六。
/// 逐字念是人耳最不易听错的读法(窗口号 06 与 6 不会混淆)。
pub fn spell_tokens(s: &str) -> String {
    s.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) => DIGIT_CN[d as usize].to_string(),
            None => c.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
