// signal.rs — 优雅退出: 捕获 SIGTERM/SIGINT, 置停止标志, 由主循环自行收尾。
//
// 为什么必须有(2026-10-11 长稳评审):
//   Rust 进程收到 SIGTERM 的默认动作是"立即终止", **析构函数一律不执行**。
//   而 ffmpeg 子进程的 kill+wait 收尸只写在 capture::PipeGrabber::drop 里。
//   于是 `systemctl restart`(配置台"一键保存"就会调它) 或手工 nohup/kill 触发的
//   每一次退出, 都会把 ffmpeg 留成孤儿继续占住 /dev/video0, 新实例启动期只能
//   靠 5s 重试兜住 —— docs/07-troubleshooting.md 把"孤儿 ffmpeg"当常见故障写,
//   其实在非 systemd 路径下它是常态。
//   现在: 信号处理函数只做一件事 —— 置原子标志; 主循环看到标志即正常返回,
//   run() 作用域内的 Source(PipeGrabber) 被析构, 收尸逻辑照常执行。
//
// 为什么处理函数里只写一个 AtomicBool:
//   信号处理函数运行在异步上下文, 只能调用"异步信号安全"的函数 —— 打日志(取锁/分配内存)、
//   加锁、释放堆内存都不安全, 可能死锁或破坏分配器状态。AtomicBool::store 是单条原子写,
//   安全。所有收尾动作都留给主循环在正常上下文里做。
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

/// 安装 SIGTERM/SIGINT/SIGHUP 处理器。幂等: 重复调用只是覆盖同一个处理函数。
/// 必须在主循环开始前调用 —— 否则启动窗口期内收到的 SIGTERM 仍会直接终止进程。
/// ★2026-10-23 中危修复: 补 SIGHUP —— SSH 断连默认终止进程, 析构不执行,
///   ffmpeg 孤儿占 /dev/video0, 与 SIGTERM/SIGINT 同根因。
pub fn install() {
    // SAFETY: signal(2) 只是把信号与一个 extern "C" 函数指针绑定。
    // 处理函数签名 (i32) -> () 与 sighandler_t 一致, 且函数体内只做一次原子写。
    // 先转 `*const ()` 再转整数: 直接 `fn_item as usize` 会被
    // `function_casts_as_integer` 拦下(不保证可移植), 两步转换是官方建议写法。
    unsafe {
        let handler = on_stop_signal as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// 是否已收到停止信号。主循环每轮开头查一次。
pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 处理函数必须能把标志翻成 true, 且此后保持 true(单调)。
    /// 直接调用处理函数而不是真发信号 —— 真发信号会打在同进程的其它测试上。
    #[test]
    fn handler_sets_and_keeps_stop_flag() {
        assert!(!stopped(), "初始必须是未停止");
        on_stop_signal(libc::SIGTERM);
        assert!(stopped(), "收到 SIGTERM 后必须置位");
        on_stop_signal(libc::SIGINT);
        assert!(stopped(), "停止标志必须单调, 不能被清回 false");
    }

    /// 信号编号是对外契约, 写死断言防上游常量被改。
    #[test]
    fn signal_numbers_are_canonical() {
        assert_eq!(libc::SIGTERM, 15);
        assert_eq!(libc::SIGINT, 2);
        assert_eq!(libc::SIGHUP, 1);
    }
}
