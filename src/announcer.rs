// announcer.rs — 专用音频线程播报: 多次播报顺序排队, 单次失败自动蜂鸣。
// 对齐 announce.py class Announcer:
//   - 入FIFO队列, 单播报线程顺序消费, 播报中来的新号自动排队不漏号不乱序;
//   - 队列积压 >= 3 说明叫号过密, 只告警不丢号(声音顺延);
//   - 开了语音但 TTS 初始化失败时蜂鸣兜底; enabled=false 是用户主动静音, 不再响。
use crate::cfg::Config;
use crate::tts::{make_tts, spell_tokens, Engine};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

type Item = (String, String);

struct Inner {
    q: Mutex<VecDeque<Item>>,
    cv: Condvar,
}

pub struct Announcer {
    inner: Arc<Inner>,
    tts_cfg: crate::cfg::Tts,
    enabled: bool,
    repeat: u32,
    template: String,
    spell: bool,
}

/// Windows 版的蜂鸣(winsound.Beep x3); Linux 生产版为 no-op,
/// 与 Python 版在 Linux 上的实际行为一致(import winsound 失败 -> except pass)。
fn beep() {}

impl Announcer {
    pub fn new(cfg: &Config) -> Arc<Announcer> {
        let t = cfg.tts.clone();
        let a = Arc::new(Announcer {
            inner: Arc::new(Inner { q: Mutex::new(VecDeque::new()), cv: Condvar::new() }),
            enabled: t.enabled,
            repeat: t.repeat,
            template: t.template.clone(),
            spell: t.spell_number,
            tts_cfg: t,
        });
        let worker = a.clone();
        std::thread::Builder::new()
            .name("announcer".into())
            .spawn(move || worker.loop_fn())
            .expect("起播报线程失败");
        a
    }

    fn loop_fn(&self) {
        let mut speak_fn: Option<Box<dyn Engine>> = None;
        if self.enabled {
            match make_tts(&self.tts_cfg) {
                Ok(e) => speak_fn = Some(e),
                Err(e) => println!("[警告] TTS初始化失败({})，仅蜂鸣提示", e),
            }
        }
        loop {
            let (number, window) = {
                let mut q = self.inner.q.lock().unwrap();
                loop {
                    if let Some(item) = q.pop_front() {
                        break item;
                    }
                    q = self.inner.cv.wait(q).unwrap();
                }
            };
            let (n, w) = if self.spell {
                (spell_tokens(&number), spell_tokens(&window))
            } else {
                (number, window)
            };
            let text = self
                .template
                .replace("{number}", &n)
                .replace("{window}", &w);
            println!("[播报] {}", text);
            match &speak_fn {
                Some(e) => {
                    if let Err(err) = e.speak(&text, self.repeat) {
                        println!("[错误] 播报失败: {}", err);
                        if self.enabled {
                            beep();
                        }
                    }
                }
                None => {
                    // 仅在"开了语音但发声函数没建起来"时蜂鸣兜底
                    if self.enabled {
                        beep();
                    }
                }
            }
        }
    }

    /// 入FIFO队列: 播报中来的新号自动排队, 顺序不乱。
    pub fn speak(&self, number: &str, window: &str) {
        let depth = self.inner.q.lock().unwrap().len();
        if depth >= 3 {
            println!("[警告] 播报队列积压 {} 条, 叫号过于密集, 声音将顺延", depth);
        }
        let mut q = self.inner.q.lock().unwrap();
        q.push_back((number.to_string(), window.to_string()));
        self.inner.cv.notify_one();
    }

    /// TTS 预热: 第一轮识别到候选时就按"将要播出的文本"向常驻合成服务发一次请求,
    /// 结果落在服务端缓存里; 等确认通过真正 speak() 时即命中缓存(~毫秒),
    /// 相当于把 1.8s 的合成耗时从关键路径上整体藏掉。
    /// 只对 piper 常驻服务有意义(其它引擎要么合成极快要么无预热手段);
    /// 后台线程执行, 不阻塞主循环; 服务不可用时静默失败(真播报时还有 CLI 回退)。
    pub fn prefetch(&self, number: &str, window: &str) {
        if !self.enabled {
            return;
        }
        // 只有 piper 常驻服务可预热; espeak 合成只要 0.07s, 无预热价值
        if !matches!(self.tts_cfg.engine.as_str(), "piper" | "auto") {
            return;
        }
        let url = match self.tts_cfg.piper_url.clone().filter(|u| !u.is_empty()) {
            Some(u) => u,
            None => return,
        };
        let (n, w) = if self.spell {
            (spell_tokens(number), spell_tokens(window))
        } else {
            (number.to_string(), window.to_string())
        };
        let text = self.template.replace("{number}", &n).replace("{window}", &w);
        println!("[预热] 后台合成: {}", text);
        std::thread::Builder::new()
            .name("tts-prefetch".into())
            .spawn(move || {
                let _ = crate::tts::http_post_wav(&url, &text);
            })
            .ok();
    }
}
