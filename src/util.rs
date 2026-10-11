// util.rs — 跨模块复用的小工具: "不中毒"的取锁、带兜底的墙钟、带超时的子进程执行。
//
// 为什么单独成模块: capture / announcer / main 都要取锁, 各自写一份 `lock().unwrap()`
// 等于把"生产路径 unwrap"这个隐患复制到每个文件(评审 P2-1)。集中一处, 语义统一,
// 将来要改也只改一个地方。run_timeout 同理 —— 播放(aplay)与启动自检(版本探测)都要
// "跑一个可能挂死的外部命令并强制超时", 各写一份等于把线程排空 + try_wait 复制两遍。
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 取锁且"不中毒"。
///
/// 为什么不用 `lock().unwrap()`: 本程序 release profile 是 `panic = "abort"`
/// (见 Cargo.toml) —— panic 直接终止进程, 锁根本不可能进入 poisoned 状态。
/// 用 `into_inner` 兜底只是把"锁坏不了"这个事实写进代码, 同时避免生产路径上
/// 多出一堆 unwrap(评审 P2-1: mutex 的 `lock().unwrap()` 应换掉)。
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 当前 Unix 时间戳(秒, 带小数)。
///
/// `duration_since(UNIX_EPOCH)` 唯一会失败的场景是系统时钟早于 1970-01-01
/// (RTC 掉电未同步、或靶机时间被错误回拨)。此时返回 0.0 而不是 unwrap 崩进程:
/// 它在本程序里只参与"间隔比较", 归零最多让第一次维护节拍提前触发一次,
/// 不影响识别/播报的正确性。
pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 带超时的子进程执行(收集 stdout/stderr)。
///
/// 超时意义: 外部命令(aplay / espeak / ffmpeg / tesseract)一旦挂死会卡死调用方,
/// 必须能强制跳过并**收尸**(kill + wait), 否则会留下 defunct 僵尸。
/// 本函数从 tts.rs 上提到这里, 供 aplay 播放与启动自检(版本探测)共用一份实现。
pub fn run_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_returns_usable_guard() {
        let m = Mutex::new(5u32);
        {
            let mut g = lock(&m);
            *g += 1;
        } // guard 在此释放
        assert_eq!(*lock(&m), 6, "guard 释放后应能再次取锁");
    }

    /// 中毒锁也必须能取到 —— 这正是 `into_inner` 兜底存在的意义。
    /// 测试 profile 恒为 unwind(cargo 忽略 `panic = "abort"`), 所以可以制造中毒。
    #[test]
    fn lock_survives_poisoned_mutex() {
        let m = Mutex::new(7u32);
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap(); // 故意用会中毒的取法
            panic!("制造中毒锁(本 panic 由 catch_unwind 捕获, 属预期)");
        }));
        assert!(poisoned.is_err(), "前提: 该锁确已中毒");
        assert!(m.is_poisoned(), "前提: 该锁确已中毒");
        assert_eq!(*lock(&m), 7, "中毒后仍应取到锁, 而不是被 unwrap 打崩");
    }

    /// 墙钟给出"合理的现代时间"。若本机时钟异常到 1970 之前, 这里也只是兜底成 0,
    /// 断言用的是"单调 + 量级"而非精确值。
    #[test]
    fn now_secs_is_plausible() {
        let t = now_secs();
        assert!(t > 1_600_000_000.0, "应晚于 2020 年: {t}");
        assert!(t < 4_100_000_000.0, "应早于 2100 年: {t}");
    }

    /// 挂死的子进程必须被超时杀掉并返回 Err(而不是把调用方一起卡死)。
    #[test]
    fn run_timeout_kills_hung_command() {
        let t0 = Instant::now();
        let mut c = Command::new("sleep");
        c.arg("30");
        let r = run_timeout(&mut c, Duration::from_millis(200));
        assert!(r.is_err(), "超时必须返回 Err");
        assert!(t0.elapsed() < Duration::from_secs(5), "不该等命令自然结束(30s)");
    }

    /// 正常命令: 收集到 stdout, 且 exit code 可读。
    #[test]
    fn run_timeout_collects_output() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo check-util-ok"]);
        let out = run_timeout(&mut c, Duration::from_secs(5)).expect("sh 应可用");
        assert!(out.status.success(), "exit code 应为 0");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("check-util-ok"),
            "应收集到 stdout"
        );
    }
}
