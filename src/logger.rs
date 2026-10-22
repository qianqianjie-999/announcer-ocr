// logger.rs — 播报日志(CSV)。可靠性约定与 Python 版一致:
// 日志写失败绝不允许带崩播报主程序 —— 文件被杀毒/预览等短暂占用时,
// 先重试(0.3s/0.6s), 仍失败则打印告警并丢弃该条日志。
use chrono::Local;
use std::io::Write;

/// Python csv.writer 最小引号转义: 含逗号/引号/换行才加引号, 引号翻倍。
/// ★2026-10-23 低危修复: 前导 = + - @ 加 ' 前缀防 Excel 公式注入 —— raw 字段
///   来自 OCR 屏幕内容原样落 CSV, Excel 打开时 "=cmd" 类单元格会被当公式执行。
fn csv_escape(s: &str) -> String {
    let s = if s.starts_with('=') || s.starts_with('+') || s.starts_with('-') || s.starts_with('@') {
        format!("'{}", s)
    } else {
        s.to_string()
    };
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// 写一条播报日志: 时间戳,号码,窗口,原始文本,状态。
/// 与 Python 版同为 utf-8-sig: 仅在新建空文件时写 BOM(Excel 打开不乱码)。
pub fn log(log_file: &str, number: &str, window: &str, raw: &str, status: &str) {
    let ts = Local::now().format("%Y-%m-%dT%H:%M:%S");
    let line = format!(
        "{},{},{},{},{}\r\n",
        csv_escape(&ts.to_string()),
        csv_escape(number),
        csv_escape(window),
        csv_escape(raw),
        csv_escape(status)
    );
    let mut last_err = String::new();
    for attempt in 0..3u32 {
        let need_bom = std::fs::metadata(log_file)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
        {
            Ok(mut f) => {
                let mut ok = true;
                if need_bom {
                    ok &= f.write_all("\u{feff}".as_bytes()).is_ok();
                }
                ok &= f.write_all(line.as_bytes()).is_ok();
                if ok {
                    return;
                }
                last_err = "写入失败".into();
            }
            Err(e) => last_err = e.to_string(),
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1) as u64));
        }
    }
    println!("[警告] 日志写入失败(已重试3次, 本条丢弃): {}", last_err);
}

/// 归档目录与文件名前缀 —— 与 deploy/announce-cleanup.sh 逐字一致。
/// 为什么必须一致: 靶机上清理脚本与主程序**同时**在做日志治理。
/// 若两边用不同命名, 归档目录里会混进两套文件, 脚本按 announce_log_*.csv 统计
/// 保留个数时就会漏算, 磁盘治理失效。
pub const DEFAULT_ARCHIVE_DIR: &str = "logs_archive";
const ARCHIVE_PREFIX: &str = "announce_log_";

/// 播报日志按大小轮转。返回 Some(归档路径) 表示本次确实轮转了。
///
/// 与清理脚本的分工: 脚本每天 03:30 兜底扫一遍(还顺手清存证图/journal/磁盘告急),
/// 但那是**按天**的 —— 一天之内把 8MB 写满就要等到第二天才被收拾, 且脚本只认固定
/// 路径 `/home/debian/anntest`。本函数让主程序自己具备"写满了就自己归档"的能力,
/// 使得 nohup 手工拉起 / 换部署路径的场景也不会写满 SD 卡。
///
/// max_bytes == 0 或 keep == 0 时不做任何事(把治理完全交给脚本)。
/// 轮转用 rename 把主文件整份搬走, 下一次 append 会重建空文件并补 BOM
/// (与脚本的 `: > "$CSV"` 效果等价, 因为 logger 每次都是 open-append 不留句柄)。
pub fn rotate_if_needed(
    log_file: &str,
    archive_dir: &str,
    max_bytes: u64,
    keep: u32,
) -> Option<String> {
    if max_bytes == 0 || keep == 0 {
        return None;
    }
    let size = match std::fs::metadata(log_file) {
        Ok(m) => m.len(),
        Err(_) => return None, // 还没有日志文件: 没什么可轮转
    };
    if size < max_bytes {
        return None;
    }
    // 归档目录建不出来就**不要**动主文件 —— 宁可继续写, 也不能把日志弄丢。
    if std::fs::create_dir_all(archive_dir).is_err() {
        println!("[警告] 日志归档目录 {} 不可用, 跳过轮转", archive_dir);
        return None;
    }
    let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let dst = format!(
        "{}/{}{}.csv",
        archive_dir.trim_end_matches('/'),
        ARCHIVE_PREFIX,
        stamp
    );
    if std::fs::rename(log_file, &dst).is_err() {
        return None;
    }
    let removed = prune_archive(archive_dir, keep);
    if removed > 0 {
        println!("[日志] 归档目录超出 {} 个, 已删除 {} 个最旧归档", keep, removed);
    }
    Some(dst)
}

/// 归档只留最近 keep 个。文件名为时间戳, 字典序 == 时间序, 无需读 mtime。
/// 只删严格匹配 `announce_log_*.csv` 的普通文件, 绝不通配符横扫。
fn prune_archive(dir: &str, keep: u32) -> usize {
    let dir = dir.trim_end_matches('/');
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                n.starts_with(ARCHIVE_PREFIX) && n.ends_with(".csv") && n.len() > ARCHIVE_PREFIX.len() + 4
            })
            .collect(),
        Err(_) => return 0,
    };
    if names.len() <= keep as usize {
        return 0;
    }
    names.sort();
    let mut removed = 0;
    for n in &names[..names.len() - keep as usize] {
        if std::fs::remove_file(format!("{}/{}", dir, n)).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::{csv_escape, prune_archive, rotate_if_needed, DEFAULT_ARCHIVE_DIR};

    /// 每个用例独立目录: 测试并发跑, 共用目录会互相踩。
    fn sandbox(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("announce_logtest_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn no_rotation_below_threshold() {
        let d = sandbox("below");
        let log = d.join("a.csv");
        std::fs::write(&log, b"x".repeat(100)).unwrap();
        let arch = d.join(DEFAULT_ARCHIVE_DIR);
        let r = rotate_if_needed(log.to_str().unwrap(), arch.to_str().unwrap(), 1000, 6);
        assert!(r.is_none(), "未达阈值不应轮转");
        assert!(log.exists(), "主文件必须原样保留");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rotation_moves_file_to_timestamped_archive() {
        let d = sandbox("rotate");
        let log = d.join("a.csv");
        std::fs::write(&log, b"x".repeat(2000)).unwrap();
        let arch = d.join(DEFAULT_ARCHIVE_DIR);
        let dst = rotate_if_needed(log.to_str().unwrap(), arch.to_str().unwrap(), 1000, 6)
            .expect("超阈值必须轮转");
        assert!(!log.exists(), "主文件应被搬走(下次 append 重建)");
        let name = std::path::Path::new(&dst).file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("announce_log_") && name.ends_with(".csv"), "归档命名: {name}");
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 2000, "归档内容不能变");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn disabled_when_threshold_zero() {
        let d = sandbox("disabled");
        let log = d.join("a.csv");
        std::fs::write(&log, b"x".repeat(5000)).unwrap();
        let arch = d.join(DEFAULT_ARCHIVE_DIR);
        assert!(rotate_if_needed(log.to_str().unwrap(), arch.to_str().unwrap(), 0, 6).is_none());
        assert!(log.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn archive_prune_keeps_newest_by_name_order() {
        let d = sandbox("prune");
        let arch = d.join(DEFAULT_ARCHIVE_DIR);
        std::fs::create_dir_all(&arch).unwrap();
        for i in 1..=9u32 {
            std::fs::write(
                arch.join(format!("announce_log_2026101{}_000000.csv", i % 10)),
                b"z",
            )
            .unwrap();
        }
        // 无关文件绝不能被删
        std::fs::write(d.join("keepme.csv"), b"z").unwrap();
        std::fs::write(arch.join("notes.txt"), b"z").unwrap();

        let removed = prune_archive(arch.to_str().unwrap(), 6);
        assert_eq!(removed, 3, "9 个归档保留 6 个");
        assert!(d.join("keepme.csv").exists());
        assert!(arch.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_log_file_is_not_an_error() {
        let d = sandbox("missing");
        let log = d.join("never_written.csv");
        let arch = d.join(DEFAULT_ARCHIVE_DIR);
        assert!(rotate_if_needed(log.to_str().unwrap(), arch.to_str().unwrap(), 1, 6).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn csv_escape_plain_untouched() {
        assert_eq!(csv_escape("GA999"), "GA999");
        assert_eq!(csv_escape("2026-10-07T18:00:00"), "2026-10-07T18:00:00");
    }

    #[test]
    fn csv_escape_quotes_on_specials() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
    }
}
