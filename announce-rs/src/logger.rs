// logger.rs — 播报日志(CSV)。可靠性约定与 Python 版一致:
// 日志写失败绝不允许带崩播报主程序 —— 文件被杀毒/预览等短暂占用时,
// 先重试(0.3s/0.6s), 仍失败则打印告警并丢弃该条日志。
use chrono::Local;
use std::io::Write;

/// Python csv.writer 最小引号转义: 含逗号/引号/换行才加引号, 引号翻倍。
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
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
