// ocr.rs — tesseract 子进程 OCR + 容错解析（正则与数字归一化逐条对齐 Python 版）
// 选型依据(实测 J1850 靶机): chi_sim 单语言 + psm 11 + ROI 裁剪原图,
// 单次 1.43s, 两张测试卡 4 个号全对; chi_sim+eng 慢 33% 无精度增益。
use crate::cfg::OcrCfg;
use regex::Regex;
use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// 单次 OCR 的超时上限。
/// ★为什么必须有: psm11(sparse) 对模糊/反光帧会病态慢 —— 靶机实测同一张图,
///   正常帧 1.5s, 反光+运动模糊帧能烧 15~20s CPU, 把主循环拖到 24s/圈,
///   连带把确认窗口(默认4s)撑爆 -> 候选永远凑不齐 2 轮 -> 永远不播报。
///   正常识别 1.5s 就够, 10s 是 6 倍余量; 超时的轮按"无候选"处理, 不计数。
const OCR_TIMEOUT_SECS: u64 = 10;

pub struct Ocr {
    bin: String,
    lang: String,
    psm: u32,
    dpi: u32,
    pats: Vec<Regex>,
    tmp: std::path::PathBuf,
    counter: std::cell::Cell<u64>,
}

impl Ocr {
    pub fn new(cfg: &OcrCfg) -> Result<Ocr, String> {
        let pats = cfg
            .patterns()
            .into_iter()
            .map(|p| Regex::new(&p).map_err(|e| format!("正则编译失败 {:?}: {}", p, e)))
            .collect::<Result<Vec<_>, _>>()?;
        if pats.is_empty() {
            return Err("config.ocr.pattern 未配置".into());
        }
        // 启动时清掉上次崩溃残留的 OCR 临时文件(实测 /tmp 下能攒一堆)
        let prefix = "announce_rs_ocr";
        if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(prefix) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(Ocr {
            bin: cfg.bin.clone(),
            lang: cfg.lang.clone(),
            psm: cfg.psm,
            dpi: cfg.dpi,
            pats,
            tmp: std::env::temp_dir().join("announce_rs_ocr.png"),
            counter: std::cell::Cell::new(0),
        })
    }

    /// 对 ROI 图做识别, 返回拼接的原始文本(多行以空格连接, 与 Python 版一致)。
    /// 超时(OCR_TIMEOUT_SECS)返回 Err —— 主循环按无候选处理, 不能让一张病态帧拖死整条链。
    pub fn recognize(&self, img: &image::RgbImage) -> Result<String, String> {
        self.recognize_psm(img, self.psm)
    }

    /// 指定 psm 的识别(兜底链会用同一张增强图分别跑 psm11/psm6, 两者错误模式不同,
    /// 靶机实测同一图 psm11 读对窗口、psm6 读对号码, 先中先用)。
    pub fn recognize_psm(&self, img: &image::RgbImage, psm: u32) -> Result<String, String> {
        // 唯一临时文件名: 唯一化的意义在于并发/重试时不会被读到半张图
        let n = self.counter.get() + 1;
        self.counter.set(n);
        let tmp = self.tmp.with_extension(format!("{}.png", n));

        let file = std::fs::File::create(&tmp).map_err(|e| format!("临时文件创建失败: {}", e))?;
        image::DynamicImage::ImageRgb8(img.clone())
            .write_to(&mut std::io::BufWriter::new(&file), image::ImageOutputFormat::Png)
            .map_err(|e| format!("PNG 编码失败: {}", e))?;
        drop(file);

        let res = self.run_tesseract(&tmp, psm);
        let _ = std::fs::remove_file(&tmp);
        res
    }

    /// 起 tesseract 子进程并限时等待。
    /// 两个关键点(都是靶机实测踩出来的):
    ///   ① OMP_THREAD_LIMIT=1 —— 服务 cgroup(CPUQuota) 下多线程 tesseract 会互相
    ///      空转抢占, 同一张图从 1.5s 恶化到 15s; 单线程反而更快更稳。
    ///   ② 必须限时 + 排空管道 —— .output() 会无限等, 挂死的子进程能卡死主循环;
    ///      stdout/stderr 不排空, 子进程写满管道后会被阻塞。
    fn run_tesseract(&self, tmp: &std::path::Path, psm: u32) -> Result<String, String> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg(tmp.to_str().unwrap_or("ann_ocr.png"))
            .arg("stdout")
            .arg("-l").arg(&self.lang)
            .arg("--psm").arg(psm.to_string())
            .arg("--dpi").arg(self.dpi.to_string())
            .env("OMP_THREAD_LIMIT", "1")
            .env("OMP_NUM_THREADS", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "tesseract 启动失败(apt install tesseract-ocr tesseract-ocr-chi-sim): {}",
                e
            )
        })?;
        // spawn 时固定了 Stdio::piped(), 正常不会为 None; 真出现就收尸返回错误,
        // 绝不用 unwrap —— panic = "abort" 下 unwrap 等于整个进程猝死。
        let (mut so, mut se) = match (child.stdout.take(), child.stderr.take()) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("tesseract 管道不可用(不可能发生)".to_string());
            }
        };
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
        let start = std::time::Instant::now();
        let status_res = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) => {
                    if start.elapsed().as_secs() >= OCR_TIMEOUT_SECS {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(format!(
                            "tesseract 超时({}s)已终止 —— 该帧疑似病态(反光/运动模糊)",
                            OCR_TIMEOUT_SECS
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => break Err(format!("tesseract 等待失败: {}", e)),
            }
        };
        // ★通道数据只能取一次: mpsc 的 recv 是消费性的, 取走即没了。
        //   (实测踩过: 先"排空"再"取结果", 第二次 recv 永远超时 -> 所有 OCR 结果
        //   都变成空串, 整个系统从此识别不到任何号码。)
        let out_bytes = rx1
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or_default();
        let err_bytes = rx2
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or_default();
        let status: std::process::ExitStatus = status_res?;
        if !status.success() {
            return Err(format!(
                "tesseract 退出码 {:?}: {}",
                status.code(),
                String::from_utf8_lossy(&err_bytes).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out_bytes)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// 配置的主 psm(兜底链第一个变体用它, 第二个固定 6)。
    pub fn default_psm(&self) -> u32 {
        self.psm
    }

    /// 数字段归一化: OCR 常把 0 读成 O、1 读成 I/l（与 Python 版 norm_digits 一致）
    fn norm_digits(s: &str) -> String {
        s.replace('O', "0").replace(['I', 'l'], "1")
    }

    /// 号码数字段只保留数字(去掉夹进来的字母噪声), 见 parse() 的注释。
    fn only_digits(s: &str) -> String {
        s.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// 解析原始文本为 (号码, 窗口) 集合。多条正则取并集; 前缀字母保留、其余归一化。
    /// 解析前先清洗 tesseract 常见噪声(靶机实测的三种稳定变异):
    ///   ① 窗口号前误插 0: "GA999到0C01" / "到003号窗口" -> "到C01"/"到03号窗口";
    ///   ② 号码前缀被读成形近数字: "6A777"->"GA777"、"64999"->"GA999";
    ///   ③ 号码中间插 ?: "GA9?99" -> "GA999"。
    /// 仅删"到/至"后紧跟大写字母的连续 0, 真窗口号"到01号窗"(纯数字)不受影响。
    pub fn parse(&self, txt: &str) -> HashSet<(String, String)> {
        let flat: String = clean_ocr_noise(txt);
        let mut out = HashSet::new();
        for p in &self.pats {
            for c in p.captures_iter(&flat) {
                // ★前导大写字母守卫(2026-10-22 第三轮审查): 生产正则的号码组是
                //   [A-Z]{1,2}\d{3,6} 这类"宽前缀"写法, 从号码中间起也能切出合法匹配
                //   —— "GAT174" 会在 A 处切出 "AT174", 杂字母 T 落进前缀, 同号变新号
                //   重复播报。旧修法指望下面的数字段清洗兜底, 但宽正则切出的数字段
                //   恒为纯数字, 该清洗对生产正则恒为空操作 —— 宣称的修复实际无效。
                //   守卫: 匹配起点前一字符是大写 A-Z ⇒ 该匹配咬掉了真前缀的尾巴,
                //   丢弃原切分, 把前一字符并回号码头交给原清洗链:
                //   "G"+"AT174" -> 前缀 GA + 数字段 "T174" -> only_digits 清掉 T
                //   -> 还原真号 GA174。行首或前一字符为中文/标点/数字的匹配照常放行
                //   (如 "青GA174" 的 "青"), 真号不受影响。
                let num_raw = match c.get(1) {
                    Some(m) => {
                        let s = m.start();
                        if s > 0 && flat.as_bytes()[s - 1].is_ascii_uppercase() {
                            // 前一字符必为单字节 ASCII 大写字母, 直接取字节拼回
                            let mut owned = String::with_capacity(m.as_str().len() + 1);
                            owned.push(flat.as_bytes()[s - 1] as char);
                            owned.push_str(m.as_str());
                            owned
                        } else {
                            m.as_str().to_string()
                        }
                    }
                    None => String::new(),
                };
                let win = c.get(2).map(|m| m.as_str()).unwrap_or("");
                let (pre, rest) = split_prefix(&num_raw);
                // 窗口号归一化后再折叠误读的前导零: "CO01" -> norm -> "C001" -> "C01"
                //
                // ★号码数字段清洗(2026-10-13 现场实测): 糊帧/过渡帧会在"前缀与数字之间"
                //   多认一个字母 —— 屏幕上的 GA174 被读成 "GAT174"。旧逻辑把 "GAT174"
                //   的不同写法当不同号码, 且宽正则会回溯切出 "AT174" 播成"诶提一七四"
                //   (听感像英语), 同一个号连播两遍。数字段只可能是数字, 夹进去的字母
                //   一律是噪声, 丢掉即还原真号(GAT174 -> GA174)。该清洗要真正生效,
                //   依赖上面的前导大写字母守卫先把被宽正则吞掉的前缀字母并回来。
                let num_digits = Self::only_digits(&Self::norm_digits(rest));
                if num_digits.is_empty() {
                    continue; // 数字段被噪声吃光 -> 这组不算候选, 别播半截号
                }
                out.insert((
                    format!("{}{}", pre, num_digits),
                    fold_window_zeros(&Self::norm_digits(win)),
                ));
            }
        }
        out
    }
}

/// 窗口号前导零折叠: 字母前缀后出现 2 个以上连续 0 时丢一个。
/// 由来: "C01" 常被读成 "CO01"(C 后误插 O), norm_digits 把 O 归一成 0 后变成
/// "C001" —— 与真窗口 C01 是不同去重键, 会导致同号重复播报(2026-10-07 实测)。
/// 真窗口 "C010"(尾零)与纯数字窗口 "03" 不受影响。
fn fold_window_zeros(s: &str) -> String {
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_uppercase() {
        return s.to_string();
    }
    let letters = b.iter().take_while(|c| c.is_ascii_uppercase()).count();
    let rest = &s[letters..];
    let zeros = rest.len() - rest.trim_start_matches('0').len();
    if zeros >= 2 {
        // 丢掉全部多余前导零, 补回一个: "C001" -> "C01"
        format!("{}0{}", &s[..letters], &rest[zeros..])
    } else {
        s.to_string()
    }
}

/// OCR 噪声清洗(见 parse 注释), 三条规则(均靶机实测总结):
///   ① 去 '?' 与全部空白;
///   ② 号码前缀数字冒充字母: "6A777"->"GA777"、"64999"->"GA999" ——
///      白字深底上 G 读成 6、A 读成 4 是稳定变异; 仅当形如"[映射字符]{1,2}+3~4位数字+
///      到/至"才替换(号码必须后跟"到"), 普通数字文本不受影响;
///   ③ 窗口号前误插 0: "到0C01"->"到C01"(0 后跟字母, 全删);
///      "到003号窗口"->"到03号窗口"(C 被读成 0, 两个 0 丢一个)。
fn clean_ocr_noise(txt: &str) -> String {
    let chars: Vec<char> = txt
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '?')
        .collect();
    let chars = promote_prefix_lookalikes(&chars);
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if c == '到' || c == '至' {
            let mut j = i;
            while j < chars.len() && chars[j] == '0' {
                j += 1;
            }
            if j > i && j < chars.len() && chars[j].is_ascii_uppercase() {
                i = j; // 误插的 0 后面跟着字母: 全部丢弃
            } else if j > i + 1 && j < chars.len() && chars[j].is_ascii_digit() {
                i += 1; // "到003": 第一个 0 是窗口字母的误读, 只丢一个
            }
        }
    }
    out
}

/// 号码前缀的"数字冒充字母"还原表(G->6 / A->4 / B->8 / S->5 / Z->2 为常见形近误读)。
/// O/0、I/1 不还原: 与号码内部的数字归一化(O->0)方向冲突, 且误还原因果严重。
const PREFIX_LOOKALIKE: &[(char, char)] = &[('6', 'G'), ('4', 'A'), ('8', 'B'), ('5', 'S'), ('2', 'Z')];

fn lookalike_letter(c: char) -> Option<char> {
    if c.is_ascii_uppercase() {
        return Some(c);
    }
    PREFIX_LOOKALIKE
        .iter()
        .find(|(d, _)| *d == c)
        .map(|(_, l)| *l)
}

/// 把"数字冒充的号码前缀"还原为字母(仅当其后紧跟 3~4 位数字且再后一位是 到/至)。
/// 例: "6A777到C03号窗口" -> "GA777到C03号窗口"; "64999到C01号" -> "GA999到C01号"。
/// 优先尝试 2 字符前缀(覆盖 "64999"->"GA999"), 再试 1 字符(覆盖 "6777"->"G777")。
/// 已全为字母的前缀(如 "GA777")不动; 普通数字(如时间戳、窗口号)因不满足
/// "后跟 到/至" 的硬条件而天然免疫。
fn promote_prefix_lookalikes(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    'outer: while i < chars.len() {
        for len in (1..=2).rev() {
            if i + len >= chars.len() {
                continue;
            }
            let seg = &chars[i..i + len];
            let mut pre = Vec::with_capacity(len);
            let mut ok = true;
            for &c in seg {
                match lookalike_letter(c) {
                    Some(m) => pre.push(m),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok || !seg.iter().any(|c| c.is_ascii_digit()) {
                continue; // 前缀里必须至少有一个数字(纯字母前缀是正常识别, 不动)
            }
            // 后随 3~4 位数字, 再后一位是 到/至
            let mut j = i + len;
            let mut n = 0;
            while j < chars.len() && chars[j].is_ascii_digit() {
                n += 1;
                j += 1;
            }
            if (3..=4).contains(&n) && j < chars.len() && (chars[j] == '到' || chars[j] == '至') {
                // ★前导大写字母守卫(2026-10-22 第三轮审查): 候选段前一位已是真前缀的
                //   大写字母时, 段里的"数字"是号码数字段的首位(如 "GA6123" 的 "A6"/"6"),
                //   不是冒充字母 —— 还原会把 "GA6123" 变形 "GAG123", 正则再切出 "AG123",
                //   前缀被改导致号码闸门放行最大号、误播错号。禁用该段还原, 原样保留。
                if i > 0 && chars[i - 1].is_ascii_uppercase() {
                    continue;
                }
                for m in pre {
                    out.push(m);
                }
                i += len;
                continue 'outer;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 取前缀大写字母(至多 2 个): "GA999" -> ("GA", "999")
fn split_prefix(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && i < 2 && b[i].is_ascii_uppercase() {
        i += 1;
    }
    s.split_at(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生产同款双正则(1~2位字母前缀), 见 _cam_test/config.json
    const PATS: &[&str] = &["([A-Z]{1,2}\\d{3,6})", "([A-Z]{1,2}\\d{3,6})[到至]([A-Z]{0,2}\\d{1,3})[号窗]"];

    fn parse(txt: &str) -> HashSet<(String, String)> {
        let cfg = OcrCfg {
            pattern: Some(crate::cfg::OneOrMany::Many(
                PATS.iter().map(|s| s.to_string()).collect(),
            )),
            ..OcrCfg::default()
        };
        Ocr::new(&cfg).unwrap().parse(txt)
    }

    fn one(res: &HashSet<(String, String)>) -> (String, String) {
        // pattern1(仅号码)会产生 (号码, "") 条目, 属正常; 只校验带窗口的条目唯一
        let with_win: Vec<_> = res.iter().filter(|(_, w)| !w.is_empty()).collect();
        assert_eq!(with_win.len(), 1, "应恰好解析出 1 组带窗口的: {res:?}");
        with_win[0].clone()
    }

    #[test]
    fn parse_two_cards_on_one_screen() {
        let r = parse("请GA999到C01号窗口办理 请GA200到C08号窗口办理");
        assert!(r.contains(&("GA999".to_string(), "C01".to_string())));
        assert!(r.contains(&("GA200".to_string(), "C08".to_string())));
    }

    #[test]
    fn parse_window_extra_zero() {
        // "CO01" 实为 C01 (C 后误插 O → norm 成 0 → 前导零折叠)
        let (num, win) = one(&parse("青GA999到CO01号窗口办理"));
        assert_eq!((num, win), ("GA999".into(), "C01".into()));
    }

    #[test]
    fn parse_window_true_trailing_zero_kept() {
        // 尾零与多前导零折叠无关
        let (_, win) = one(&parse("请GA777到C10号窗口办理"));
        assert_eq!(win, "C10");
    }

    #[test]
    fn parse_pure_digit_window_kept() {
        let (num, win) = one(&parse("请GA777到03号窗口办理"));
        assert_eq!((num, win), ("GA777".into(), "03".into()));
    }

    #[test]
    fn parse_inserted_zero_after_dao() {
        let (num, win) = one(&parse("青GA999到0C01号窗"));
        assert_eq!((num, win), ("GA999".into(), "C01".into()));
    }

    #[test]
    fn parse_prefix_lookalike_digits() {
        // 6→G / 64→GA 前缀冒充
        for (raw, want) in [("6A777到C03号窗", "GA777"), ("64999到C01号窗", "GA999")] {
            let (num, _) = one(&parse(raw));
            assert_eq!(num, want, "{raw}");
        }
    }

    #[test]
    fn parse_question_noise_in_number() {
        let (num, win) = one(&parse("青GA9?99到C01号窗"));
        assert_eq!((num, win), ("GA999".into(), "C01".into()));
    }

    #[test]
    fn parse_guard_restores_true_prefix_from_stray_letter() {
        // ★第三轮审查回归: 宽正则会从 "GAT174" 的 A 处切出 "AT174"(杂字母 T 落进
        //   前缀, 旧修法 only_digits 对此恒空操作) → 同号变新号重复播报。
        //   守卫须把前一字符并回号码头, 走原清洗链还原真号 GA174, 窗口照常解析。
        let (num, win) = one(&parse("请GAT174到C04号窗"));
        assert_eq!((num, win), ("GA174".into(), "C04".into()));
    }

    #[test]
    fn parse_promote_guard_keeps_four_digit_number() {
        // ★第三轮审查回归: "GA6123" 的首位 6 曾被当"数字冒充字母"还原 → "GAG123"
        //   再被正则切出 "AG123", 前缀被改导致号码闸门放行最大号、误播错号。
        //   真号必须原样保留。
        let (num, win) = one(&parse("GA6123到C01号窗"));
        assert_eq!((num, win), ("GA6123".into(), "C01".into()));
    }

    #[test]
    fn fold_window_zeros_cases() {
        assert_eq!(fold_window_zeros("C001"), "C01");
        assert_eq!(fold_window_zeros("C008"), "C08");
        assert_eq!(fold_window_zeros("C01"), "C01");  // 单 0 不动
        assert_eq!(fold_window_zeros("C10"), "C10");  // 尾零不动
        assert_eq!(fold_window_zeros("03"), "03");    // 纯数字窗口不动
        assert_eq!(fold_window_zeros(""), "");
    }

    #[test]
    fn norm_digits_lookalikes() {
        assert_eq!(Ocr::norm_digits("C0I1"), "C011");
        assert_eq!(Ocr::norm_digits("8O0l"), "8001");
        assert_eq!(Ocr::norm_digits("123"), "123");
    }
}
