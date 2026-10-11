// selfcheck.rs — 启动自检: 探测关键外部依赖是否可用, 并把版本打进日志。
//
// 为什么要做(评审 P2-5): 线上"换个靶机就变慢 / 识别全空"这类问题, 九成是
// ffmpeg / tesseract 版本不同, 或者干脆没装。启动时把版本记一笔, 现场排障
// 一眼就能对上号, 不用等"跑了一整天不出声"再回来猜。
//
// 只做"记录 + 缺谁告警", 不做"缺了就退出": 取流依赖 ffmpeg, 但 simulate 模式
// 不需要它; OCR 依赖 tesseract, 缺了也只是识别为空。让程序照常起, 把事实写清楚。
use crate::util::run_timeout;
use std::process::Command;
use std::time::Duration;

/// 版本探测的超时: ffmpeg/tesseract 打印版本都是毫秒级, 5s 是防"二进制损坏挂死"。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 跑一次 `bin args...`, 返回输出的第一行非空文本。
/// ffmpeg/tesseract 有的把版本写 stdout、有的写 stderr, 两边都看;
/// 找不到二进制或没有任何输出时返回 None(不 panic)。
pub fn probe_version(bin: &str, args: &[&str]) -> Option<String> {
    let mut c = Command::new(bin);
    c.args(args);
    let out = run_timeout(&mut c, PROBE_TIMEOUT).ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// 启动自检入口: 打印 ffmpeg / tesseract 的版本或缺失告警。
pub fn report() {
    const DEPS: [(&str, &[&str]); 2] = [
        ("ffmpeg", &["-version"]),
        ("tesseract", &["--version"]),
    ];
    for (bin, args) in DEPS {
        match probe_version(bin, args) {
            Some(v) => println!("[自检] {} 可用: {}", bin, v),
            None => println!(
                "[自检] 警告: 未探测到 {} (取流/OCR 相关功能会受影响, 请确认已安装并在 PATH 中)",
                bin
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 能正常执行的命令: 取到输出的第一行非空文本(跳过前导空行)。
    /// 用 `sh -c` 而不是假想某个具体工具 —— CI 上一定有 sh。
    #[test]
    fn probe_reads_first_nonempty_line() {
        let v = probe_version("sh", &["-c", "printf '\\n'; echo announce-probe-ok"]);
        assert_eq!(v.as_deref(), Some("announce-probe-ok"));
    }

    /// 不存在的二进制: 返回 None, 而不是 panic 或空字符串。
    #[test]
    fn probe_missing_binary_is_none() {
        assert!(probe_version("definitely-not-a-real-binary-xyz", &[]).is_none());
    }

    /// 真机依赖存在时 report() 不应 panic(顺带把版本打进测试输出便于排障)。
    #[test]
    fn report_does_not_panic() {
        report();
    }
}
