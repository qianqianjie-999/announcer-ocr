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

/// 取锁且"不中毒"。
/// 为什么不用 `lock().unwrap()`: 本程序 release profile 是 `panic = "abort"`,
/// panic 直接终止进程, 锁根本不可能进入 poisoned 状态 —— 用 `into_inner` 兜底
/// 只是把"锁坏不了"这个事实写进代码, 同时避免生产路径上多出一堆 unwrap
/// (评审 P2-1: mutex 的 lock().unwrap() 应换掉)。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Inner {
    q: Mutex<VecDeque<Item>>,
    cv: Condvar,
}

pub struct Announcer {
    inner: Arc<Inner>,
    /// 是否正在出声(已出队但还没播完)。优雅退出时用它判断"能不能走了"。
    busy: Arc<Mutex<bool>>,
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
            busy: Arc::new(Mutex::new(false)),
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
                let mut q = lock(&self.inner.q);
                loop {
                    if let Some(item) = q.pop_front() {
                        break item;
                    }
                    q = self.inner
                        .cv
                        .wait(q)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            *lock(&self.busy) = true; // 出队即标记"在出声"
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
            *lock(&self.busy) = false;
        }
    }

    /// 队列是否空闲(没有待播的, 也没有正在出声的)。
    pub fn is_idle(&self) -> bool {
        lock(&self.inner.q).is_empty() && !*lock(&self.busy)
    }

    /// 优雅退出: 等播报队列排空(有界), 别在重启/停机时把已确认的叫号掐掉。
    /// 返回 true = 已排空; false = 超时仍有积压(打印告警由调用方负责)。
    /// 不负责停止播报线程 —— 进程退出时它自然结束。
    pub fn drain(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.is_idle() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// 入FIFO队列: 播报中来的新号自动排队, 顺序不乱。
    pub fn speak(&self, number: &str, window: &str) {
        let depth = lock(&self.inner.q).len();
        if depth >= 3 {
            println!("[警告] 播报队列积压 {} 条, 叫号过于密集, 声音将顺延", depth);
        }
        let mut q = lock(&self.inner.q);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// enabled=false: 不建 TTS 引擎(否则会去探测 espeak/piper, 依赖靶机环境),
    /// 只验证队列与"队列忙闲"语义 —— 这正是优雅退出依赖的那部分。
    fn silent() -> Config {
        let mut c = Config::default();
        c.tts.enabled = false;
        c.tts.template = "T{number}-{window}".to_string();
        c
    }

    #[test]
    fn fresh_announcer_is_idle() {
        let a = Announcer::new(&silent());
        assert!(a.is_idle(), "刚起来没有待播项");
        assert!(a.drain(Duration::from_millis(100)), "空队列应立即排空");
    }

    #[test]
    fn drain_waits_until_queue_is_consumed() {
        let a = Announcer::new(&silent());
        for i in 0..5u32 {
            a.speak(&format!("GA20{i}"), "C03");
        }
        assert!(a.drain(Duration::from_secs(5)), "排队项应被播报线程消费完");
        assert!(a.is_idle(), "排空后应为空闲");
    }
}
