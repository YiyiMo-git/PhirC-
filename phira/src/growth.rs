//! 玩家成长树：从游玩历史（history.json）算出各枝的解锁进度。
//!
//! 纯只读统计——不写任何文件。树结构是固定 6 枝，每枝 3-5 个节点，
//! 按「游玩量 / 精度 / 挑战 / 连击 / 探索 / 坚持」六个维度刻画玩家成长。

use serde_json::{json, Value};

use crate::history;

/// 连续游玩天数（按自然日去重后的最长连续段）。
fn max_streak(days: &[String]) -> u32 {
    if days.is_empty() {
        return 0;
    }
    let mut days: Vec<&String> = days.iter().collect();
    days.sort();
    days.dedup();
    use chrono::NaiveDate;
    let parsed: Vec<NaiveDate> = days.iter().filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).collect();
    let mut best = 1u32;
    let mut cur = 1u32;
    for w in parsed.windows(2) {
        let diff = (w[1] - w[0]).num_days();
        if diff == 1 {
            cur += 1;
            best = best.max(cur);
        } else if diff > 1 {
            cur = 1;
        }
    }
    best
}

/// 组装成长树数据（桥命令 `get_growth` 用）。
pub fn growth_data() -> Value {
    let records = history::all();
    let total_plays = records.len() as u32;
    let distinct: std::collections::HashSet<&str> = records.iter().map(|r| r.key.as_str()).collect();
    let distinct_songs = distinct.len() as u32;

    // 精度 / 连击 / 挑战：遍历每条记录
    let mut best_acc = 0f64;
    let mut fc_count = 0u32;
    let mut ap_count = 0u32;
    let mut best_combo = 0u32;
    // 「无 Miss 通过」的最高定数（难度字段可信才计）
    let mut max_diff_cleared = 0f32;
    let mut max_diff_fc = 0f32;
    for r in &records {
        best_acc = best_acc.max(r.accuracy);
        best_combo = best_combo.max(r.max_combo);
        if r.is_full_combo() {
            fc_count += 1;
            if r.difficulty > max_diff_fc {
                max_diff_fc = r.difficulty;
            }
        }
        if r.is_all_perfect() {
            ap_count += 1;
        }
        if r.num_of_notes > 0 && r.counts[3] == 0 && r.difficulty > 0. {
            if r.difficulty > max_diff_cleared {
                max_diff_cleared = r.difficulty;
            }
        }
    }

    // 坚持：按日期去重的游玩天数 + 连续段
    use chrono::{Local, TimeZone};
    let mut days: Vec<String> = records
        .iter()
        .map(|r| Local.timestamp_millis_opt(r.time).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default())
        .filter(|d| !d.is_empty())
        .collect();
    days.sort();
    days.dedup();
    let play_days = days.len() as u32;
    let streak = max_streak(&days);

    let stats = json!({
        "totalPlays": total_plays,
        "distinctSongs": distinct_songs,
        "playDays": play_days,
        "streak": streak,
        "bestAcc": best_acc,
        "fcCount": fc_count,
        "apCount": ap_count,
        "bestCombo": best_combo,
        "maxDiffCleared": max_diff_cleared,
    });

    // 节点构造辅助
    let node = |title: &str, desc: &str, need: &str, value: f64, goal: f64| -> Value {
        let unlocked = value >= goal;
        json!({
            "title": title,
            "desc": desc,
            "need": need,
            "value": (value * 100.).round() / 100.,
            "goal": goal,
            "unlocked": unlocked,
            "progress": if goal > 0. { ((value / goal).min(1.) * 100.).round() / 100. } else { 1. },
        })
    };

    let branches = json!([
        { "name": "磨谱", "icon": "▶", "nodes": [
            node("初次登台", "完成第一次游玩", "1 次游玩", total_plays as f64, 1.),
            node("小有手感", "累计游玩 25 次", "25 次游玩", total_plays as f64, 25.),
            node("磨谱之人", "累计游玩 100 次", "100 次游玩", total_plays as f64, 100.),
            node("百战老将", "累计游玩 300 次", "300 次游玩", total_plays as f64, 300.),
        ]},
        { "name": "精度", "icon": "◎", "nodes": [
            node("踩上节奏", "单次 ACC 达到 90%", "90.00%", best_acc, 0.90),
            node("稳定输出", "单次 ACC 达到 95%", "95.00%", best_acc, 0.95),
            node("精密制导", "单次 ACC 达到 98%", "98.00%", best_acc, 0.98),
            node("完美主义", "单次 ACC 达到 99.5%", "99.50%", best_acc, 0.995),
        ]},
        { "name": "挑战", "icon": "▲", "nodes": [
            node("入门考核", "无 Miss 通过定数 8 以上的谱面", "定数 8", max_diff_cleared as f64, 8.),
            node("进阶试炼", "无 Miss 通过定数 12 以上的谱面", "定数 12", max_diff_cleared as f64, 12.),
            node("高手之路", "无 Miss 通过定数 14 以上的谱面", "定数 14", max_diff_cleared as f64, 14.),
            node("顶尖领域", "无 Miss 通过定数 15.5 以上的谱面", "定数 15.5", max_diff_cleared as f64, 15.5),
        ]},
        { "name": "连击", "icon": "∞", "nodes": [
            node("第一次全连", "拿到 1 次 Full Combo", "1 次 FC", fc_count as f64, 1.),
            node("连击收藏家", "拿到 5 次 Full Combo", "5 次 FC", fc_count as f64, 5.),
            node("无懈可击", "拿到 20 次 Full Combo", "20 次 FC", fc_count as f64, 20.),
            node("收歌大师", "拿到 1 次全 Perfect（AP）", "1 次 AP", ap_count as f64, 1.),
        ]},
        { "name": "探索", "icon": "✦", "nodes": [
            node("初探曲库", "游玩过 3 首不同的曲目", "3 首", distinct_songs as f64, 3.),
            node("博览群谱", "游玩过 10 首不同的曲目", "10 首", distinct_songs as f64, 10.),
            node("见多识广", "游玩过 25 首不同的曲目", "25 首", distinct_songs as f64, 25.),
            node("曲库通", "游玩过 50 首不同的曲目", "50 首", distinct_songs as f64, 50.),
        ]},
        { "name": "坚持", "icon": "◷", "nodes": [
            node("第二天", "连续 2 天有游玩记录", "2 天", streak as f64, 2.),
            node("一周之约", "连续 5 天有游玩记录", "5 天", streak as f64, 5.),
            node("习惯成自然", "连续 14 天有游玩记录", "14 天", streak as f64, 14.),
            node("节奏即生活", "连续 30 天有游玩记录", "30 天", streak as f64, 30.),
        ]},
    ]);

    json!({ "ok": true, "stats": stats, "branches": branches })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn streak_counts_consecutive_days() {
        // 连续 3 天（10-01..10-03），中间断一天，再来连续 2 天
        assert_eq!(max_streak(&days(&["2026-10-01", "2026-10-02", "2026-10-03", "2026-10-05", "2026-10-06"])), 3);
        // 单日
        assert_eq!(max_streak(&days(&["2026-10-01"])), 1);
        // 空
        assert_eq!(max_streak(&[]), 0);
        // 重复日期去重（同一天打多次只算一天）
        assert_eq!(max_streak(&days(&["2026-10-01", "2026-10-01", "2026-10-02"])), 2);
        // 乱序也要正确
        assert_eq!(max_streak(&days(&["2026-10-03", "2026-10-01", "2026-10-02"])), 3);
        // 跨月连续
        assert_eq!(max_streak(&days(&["2026-09-30", "2026-10-01"])), 2);
    }
}
