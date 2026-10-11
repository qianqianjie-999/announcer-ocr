// announcer.rs — 专用音频线程播报: 多次播报顺序排队, 单次失败自动蜂鸣。
// 对齐 announce.py class Announcer:
//   - 入FIFO队列, 单播报线程顺序消费, 播报中来的新号自动排队不漏号不乱序;
//   - 队列积压 >= 3 说明叫号过密, 只告警不丢号(声音顺延);
//   - 开了语音但 TTS 初始化失败时蜂鸣兜底; enabled=false 是用户主动静音, 不再响。
use crate::cfg::Config;
use crate::tts::{make_tts, spell_tokens, Engine};
use crate::util::lock; // 取锁统一走 util(不中毒版, 详见 util.rs)
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

type Item = (String, String);

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

/// 连续"播报失败"达到多少次后重建 TTS 引擎。
///
/// 为什么需要自愈: 引擎只在启动时构建一次, 而它依赖的外部环境会变 —— 声卡被
/// pipewire/pulse 抢走、piper 常驻服务重启、USB 声卡热插拔。旧实现下一次失效就
/// "哑到下次人工重启服务"。重建 = 把探测逻辑再跑一遍(make_tts 会重新 which 可执行
/// 文件、重试连常驻服务), 通常即可恢复, 现场不必等人。
const REBUILD_AFTER_FAILS: u32 = 3;

/// 是否该重建引擎。抽成纯函数是为了可测 —— 自愈逻辑本身依赖真实 TTS 环境,
/// 但"什么时候触发"这条判据不该依赖环境。
fn should_rebuild(fails: u32, enabled: bool) -> bool {
    // enabled=false 是用户主动静音: 不建引擎, 也不该反复探测空烧 CPU。
    enabled && fails >= REBUILD_AFTER_FAILS
}


impl Announcer {
    /// 构造并启动唯一的播报线程。
    ///
    /// 返回 Result 而不是在 spawn 失败时 panic: `panic = "abort"` 下 panic 会直接
    /// 终止整个进程 —— 为"起不了一条线程"赔上识别主循环不值得。把失败交给调用方
    /// 在启动期明确报错, 比"跑起来但永远不出声"清楚得多。
    pub fn new(cfg: &Config) -> Result<Arc<Announcer>, String> {
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
            .map_err(|e| format!("起播报线程失败: {}", e))?;
        Ok(a)
    }

    fn loop_fn(&self) {
        let mut speak_fn: Option<Box<dyn Engine>> = None;
        // 连续播报失败计数。成功一次即清零 —— 判据是"连续", 偶发单次失败不触发重建。
        let mut fails: u32 = 0;
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
                Some(e) => match e.speak(&text, self.repeat) {
                    Ok(()) => fails = 0,
                    Err(err) => {
                        fails = fails.saturating_add(1);
                        println!("[错误] 播报失败(连续第 {} 次): {}", fails, err);
                        if self.enabled {
                            beep();
                        }
                    }
                },
                None => {
                    // "开了语音却没有引擎"同样计入失败: 启动时 TTS 不可用、之后环境
                    // 恢复(piper 服务起来/声卡就绪)时能自动接上, 不必等人工重启。
                    if self.enabled {
                        fails = fails.saturating_add(1);
                        beep();
                    }
                }
            }
            // —— 引擎自愈 ————————————————————————————————————————————
            // 连续失败到阈值就重新探测一次。无论成败都复位计数: 成功则彻底恢复;
            // 失败则再攒一轮, 避免"每次失败都探测"的抖动(探测本身要起子进程)。
            if should_rebuild(fails, self.enabled) {
                match make_tts(&self.tts_cfg) {
                    Ok(e) => {
                        println!("[TTS] 连续 {} 次播报失败, 已重建引擎: {}", fails, e.name());
                        speak_fn = Some(e);
                    }
                    Err(e) => {
                        println!("[TTS] 连续 {} 次播报失败, 重建仍失败({}), 继续蜂鸣兜底", fails, e);
                    }
                }
                fails = 0;
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

    /// 自愈触发判据: 连续失败到阈值才重建, 且仅在"开了语音"时。
    #[test]
    fn rebuild_triggers_only_after_consecutive_failures() {
        assert!(!should_rebuild(0, true), "零失败不该重建");
        assert!(!should_rebuild(REBUILD_AFTER_FAILS - 1, true), "差一次不重建");
        assert!(should_rebuild(REBUILD_AFTER_FAILS, true), "到阈值必须重建");
        assert!(should_rebuild(REBUILD_AFTER_FAILS + 10, true), "超额也要重建");
        assert!(!should_rebuild(u32::MAX, false), "主动静音时永不重建");
    }

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
        let a = Announcer::new(&silent()).expect("测试环境应能起播报线程");
        assert!(a.is_idle(), "刚起来没有待播项");
        assert!(a.drain(Duration::from_millis(100)), "空队列应立即排空");
    }

    #[test]
    fn drain_waits_until_queue_is_consumed() {
        let a = Announcer::new(&silent()).expect("测试环境应能起播报线程");
        for i in 0..5u32 {
            a.speak(&format!("GA20{i}"), "C03");
        }
        assert!(a.drain(Duration::from_secs(5)), "排队项应被播报线程消费完");
        assert!(a.is_idle(), "排空后应为空闲");
    }

    /// 引擎永远建不起来时, 自愈路径必须"照常消费队列、不死锁、不 panic":
    /// 每次播报计入失败 -> 到阈值触发一次重建(仍失败) -> 复位计数继续。
    /// 这是"无引擎 + enabled"这条最容易写错(在锁内重建/卡在蜂鸣)的分支。
    #[test]
    fn self_heal_path_survives_when_no_engine_available() {
        let mut c = Config::default();
        c.tts.enabled = true; // 强制走引擎探测路径
        c.tts.engine = "piper".into(); // 只试 piper, 不去探测本机 espeak
        c.tts.piper_url = Some(String::new());
        c.tts.piper_bin = Some("/nonexistent/piper".into());
        c.tts.piper_model = Some("/nonexistent/model.onnx".into());
        c.tts.template = "T{number}-{window}".into();
        let a = Announcer::new(&c).expect("线程应能启动");
        // 多播一次: 保证至少跨过阈值、触发一轮重建
        for i in 0..(REBUILD_AFTER_FAILS + 1) {
            a.speak(&format!("GA{i}"), "C01");
        }
        assert!(
            a.drain(Duration::from_secs(5)),
            "无引擎时也应把队列消费完(蜂鸣兜底), 而不是卡在重建里"
        );
    }
}
