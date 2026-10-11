// dedupe.rs — 带"时间维度"的去重表: 记住"什么时候记下的", 并按时清理。
//
// 背景(2026-10-11 长稳评审):
//   main.rs 的 `announced`(已播报集合)原先只 insert 不删除, 而叫号号码每天循环复用。
//   进程连续跑过几天后, 与前几天重复的 (号码, 窗口) 会被永久判为"已播报"而**静默不再出声**,
//   且不产生任何告警 —— 典型的软故障。现场没暴露, 只是因为 deploy 层的
//   announce-worktime-stop.timer 每天 18:00 停服、次日开服"顺带"清空了集合。
//   也就是说: 缺陷被重启掩盖着, 改成 7×24 连跑立刻显形。
//
// 本模块把"何时记下"变成数据结构的一部分, 使清理不再依赖进程重启。
// `cand_raw`(候选原文缓存) 同样是只增不删的表, 一并迁到这里治理。
use std::collections::HashMap;
use std::hash::Hash;

/// 过期策略。由配置 `announced_ttl_sec` 决定。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Expiry {
    /// 永不清理(旧行为, 仅供回归对照)。
    Never,
    /// 滚动 TTL: 距最后一次写入超过 N 秒即失效。
    Rolling(f64),
    /// 每日本地零点清零 —— 与"号码按天复用"的业务语义一致。
    DailyMidnight,
}

impl Expiry {
    /// 配置值 -> 策略。0 = 每日零点(默认), >0 = 滚动 TTL, <0 = 永不清理。
    pub fn from_ttl_sec(ttl: f64) -> Expiry {
        if ttl > 0.0 {
            Expiry::Rolling(ttl)
        } else if ttl < 0.0 {
            Expiry::Never
        } else {
            Expiry::DailyMidnight
        }
    }

    /// 当前时刻的保留下界: 早于它的条目应予清理。None = 从不清理。
    pub fn cutoff(&self, now: f64) -> Option<f64> {
        match *self {
            Expiry::Never => None,
            Expiry::Rolling(ttl) => Some(now - ttl),
            Expiry::DailyMidnight => Some(midnight_of(now)),
        }
    }
}

/// 取 now(Unix 秒)所在"本地日"的零点时间戳。
/// 时区数据取不到时返回 0.0 —— 语义等价于"不清理": 宁可多记一份, 不可误删。
fn midnight_of(now: f64) -> f64 {
    use chrono::{Local, TimeZone};
    let dt = match Local.timestamp_opt(now as i64, 0).single() {
        Some(d) => d,
        None => return 0.0,
    };
    let naive = match dt.date_naive().and_hms_opt(0, 0, 0) {
        Some(n) => n,
        None => return 0.0,
    };
    match naive.and_local_timezone(Local).single() {
        Some(m) => m.timestamp() as f64,
        None => 0.0,
    }
}

/// 带时间戳的键值表。`cap` 是硬上限兜底(防配置写错, 例如填了"永不清理"又天天换号)。
#[derive(Debug)]
pub struct AgedMap<K, V> {
    map: HashMap<K, (V, f64)>,
    expiry: Expiry,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> AgedMap<K, V> {
    pub fn new(expiry: Expiry, cap: usize) -> Self {
        AgedMap { map: HashMap::new(), expiry, cap }
    }

    pub fn contains_key(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        self.map.get(k).map(|(v, _)| v)
    }

    /// 写入/刷新一条记录, 并把写入时刻记下来(TTL 与每日零点判断都以它为准)。
    pub fn insert(&mut self, k: K, v: V, now: f64) {
        self.map.insert(k, (v, now));
    }

    /// 现存条数(维护日志用)。刻意不叫 `len`: AgedMap 是"表"而不是容器,
    /// 再配一个 `is_empty` 只会诱导调用方把"表为空"当成"可以短路"。
    pub fn count(&self) -> usize {
        self.map.len()
    }

    /// 按时效清理, 返回被清掉的条数(供调用方打日志 —— 让隐式清理变得可见)。
    pub fn prune(&mut self, now: f64) -> usize {
        let mut removed = 0usize;
        if let Some(cut) = self.expiry.cutoff(now) {
            self.map.retain(|_, (_, t)| {
                if *t < cut {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        if self.cap > 0 && self.map.len() > self.cap {
            // 保留最新的 cap 条(并列时间戳一并保留, 所以结果可能略多于 cap, 仍远小于原规模)
            let mut ts: Vec<f64> = self.map.values().map(|(_, t)| *t).collect();
            ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let floor = ts[ts.len() - self.cap];
            let before = self.map.len();
            self.map.retain(|_, (_, t)| *t >= floor);
            removed += before - self.map.len();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn key(n: &str, w: &str) -> (String, String) {
        (n.to_string(), w.to_string())
    }

    /// 本地某时刻的 Unix 秒。
    fn ts(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> f64 {
        Local
            .with_ymd_and_hms(y, m, d, hh, mm, ss)
            .single()
            .expect("本地时间必须唯一")
            .timestamp() as f64
    }

    #[test]
    fn ttl_zero_means_daily_midnight() {
        assert_eq!(Expiry::from_ttl_sec(0.0), Expiry::DailyMidnight);
        assert_eq!(Expiry::from_ttl_sec(-1.0), Expiry::Never);
        assert_eq!(Expiry::from_ttl_sec(3600.0), Expiry::Rolling(3600.0));
    }

    #[test]
    fn cutoff_is_the_local_day_start() {
        let nine = ts(2026, 10, 11, 9, 30, 0);
        let cut = Expiry::DailyMidnight.cutoff(nine).unwrap();
        assert!((cut - ts(2026, 10, 11, 0, 0, 0)).abs() < 0.5, "下界应为当天零点, 实际 {cut}");
        // 昨天 23:59 早于今天零点 -> 应被清理; 今天 00:00 起保留
        assert!(ts(2026, 10, 10, 23, 59, 0) < cut);
        assert!(ts(2026, 10, 11, 0, 0, 1) >= cut);
    }

    #[test]
    fn rolling_cutoff_follows_now() {
        let cut = Expiry::Rolling(600.0).cutoff(10_000.0).unwrap();
        assert!((cut - 9_400.0).abs() < 0.001);
        assert!(Expiry::Never.cutoff(10_000.0).is_none());
    }

    /// 核心回归: 旧实现下"次日的同一号码"被永久判为已播报 -> 静默哑掉。
    #[test]
    fn regression_next_day_same_number_becomes_announceable_again() {
        let d1 = ts(2026, 10, 11, 10, 0, 0);
        let d2 = ts(2026, 10, 12, 10, 0, 0);
        let mut a: AgedMap<(String, String), ()> =
            AgedMap::new(Expiry::from_ttl_sec(0.0), 4096);

        a.insert(key("GA200", "C03"), (), d1);
        assert!(a.contains_key(&key("GA200", "C03")), "当天必须去重");

        assert_eq!(a.prune(d2), 1, "跨天应清掉 1 条");
        assert!(!a.contains_key(&key("GA200", "C03")), "跨天后同一号码必须能重新播报");
    }

    #[test]
    fn same_day_entries_survive_prune() {
        let mut a: AgedMap<(String, String), ()> =
            AgedMap::new(Expiry::from_ttl_sec(0.0), 4096);
        let morning = ts(2026, 10, 11, 8, 0, 0);
        let afternoon = ts(2026, 10, 11, 16, 0, 0);
        a.insert(key("GA201", "C01"), (), morning);
        assert_eq!(a.prune(afternoon), 0);
        assert_eq!(a.count(), 1);
    }

    #[test]
    fn rolling_ttl_expires_only_stale_entries() {
        let mut a: AgedMap<(String, String), ()> =
            AgedMap::new(Expiry::from_ttl_sec(600.0), 4096);
        a.insert(key("OLD", "01"), (), 1_000.0);
        a.insert(key("NEW", "01"), (), 1_500.0);
        assert_eq!(a.prune(1_700.0), 1);
        assert!(!a.contains_key(&key("OLD", "01")));
        assert!(a.contains_key(&key("NEW", "01")));
    }

    /// 永不清理策略下仍有硬上限, 防内存被吃光。
    #[test]
    fn hard_cap_evicts_oldest() {
        let mut a: AgedMap<(String, String), ()> = AgedMap::new(Expiry::Never, 3);
        for i in 0..10 {
            a.insert(key(&format!("N{i}"), "01"), (), 1_000.0 + f64::from(i));
        }
        assert_eq!(a.count(), 10, "上限只在 prune 时生效");
        assert_eq!(a.prune(2_000.0), 7);
        assert_eq!(a.count(), 3);
        assert!(a.contains_key(&key("N9", "01")), "保留最新的");
        assert!(!a.contains_key(&key("N0", "01")), "淘汰最旧的");
    }

    #[test]
    fn value_is_readable_back() {
        let mut a: AgedMap<(String, String), String> =
            AgedMap::new(Expiry::DailyMidnight, 4096);
        a.insert(key("GA200", "C03"), "GA200到C03号窗口".to_string(), 1_000.0);
        assert_eq!(a.get(&key("GA200", "C03")).map(String::as_str), Some("GA200到C03号窗口"));
        assert_eq!(a.get(&key("GA999", "C01")), None);
    }
}
