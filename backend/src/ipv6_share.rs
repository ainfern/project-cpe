/*
 * @FilePath: /udx710-backend/backend/src/ipv6_share.rs
 *
 * Copyright (c) 2026 by 1orz, All Rights Reserved.
 */
//! IPv6 共享上网自愈模块
//!
//! 解决 USB 共享（sipa_eth0 蜂窝 ↔ usb0 PC 侧）IPv6 的三类状态漂移：
//!
//! 1. **地址堆积**：蜂窝 PDN 前缀每次变化（重附着/掉网重连），共享层会为 usb0
//!    新增一个由当前前缀派生的全局地址，但旧前缀地址从不删除。
//!    实机曾观察到 usb0 上堆积 6 个 `valid_lft forever` 的陈旧 /128。
//! 2. **路由重复**：同一 /64、同一 default 被反复 `add` 而不去重（主表出现 ×2）。
//! 3. **回程路径失效**：PC→蜂窝方向依赖 `iif` 策略规则（table 181/200），规则丢失
//!    或表内容漂移时回程不通。
//!
//! 自愈策略（探测驱动、幂等、非破坏）：
//! - **forwarding 确保**：all/usb0/sipa_eth0 三处 IPv6 转发开启
//! - **规则保障**：`iif sipa_eth0→181(prio422)`、`iif usb0→181(prio442)`、
//!   `lookup 200(prio2000)` 三条策略规则，priority 与 connman 保持一致
//! - **地址卫生**：usb0 全局地址只保留"当前蜂窝前缀 + 稳定 IID"；
//!   **先补后清**（前缀切换无空窗），重建受 120s 冷却保护
//! - **回程探测**：`ip -6 route get <前缀>::2 from <伪源> iif sipa_eth0`
//!   必须命中 `dev usb0` + `table 181`，否则触发路由修复
//! - **路由卫生**：main/181/200 表当前前缀路由去重补齐、陈旧前缀删除、
//!   手工 default 去重、181 表回程 default 保障
//!
//! **绝不执行 iptables/ip6tables -F**，不触碰防火墙。
//!
//! 注：路由去重采用「全删再加」而非 `ip route replace` —— 实测本设备
//! busybox 1.27 的 `replace` 不做原子替换、反而会追加重复条目。

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

/// PC 侧共享接口
const USB_IF: &str = "usb0";
/// 蜂窝上行接口
const WAN_IF: &str = "sipa_eth0";
/// 共享回程策略路由表
const TETHER_TABLE: &str = "181";
/// connman 前缀路由表
const CONNMAN_TABLE: &str = "200";
/// 需要同步当前前缀路由的附加表
const EXTRA_TABLES: &[&str] = &[CONNMAN_TABLE];

/// 策略规则优先级（与 connman 既有规则一致）
const RULE_PRIO_IIF_WAN: &str = "422";
const RULE_PRIO_IIF_USB: &str = "442";
const RULE_PRIO_LOOKUP_200: &str = "2000";

/// 回程探测使用的模拟源地址
const PROBE_SRC: &str = "2001:4860:4860::8888";
/// 设备自身出口探测目标（与 /api/connectivity 的 v6 目标一致）
const EGRESS_PROBE_TARGET: &str = "2400:3200::1";

/// 自愈周期（秒）
const HEAL_INTERVAL_SECS: u64 = 30;
/// 启动后首轮延迟（等 modem/connman 稳定）
const HEAL_INITIAL_DELAY_SECS: u64 = 15;
/// 地址重建冷却（防前缀抖动反复重建）
const ADDR_RESTORE_COOLDOWN: Duration = Duration::from_secs(120);

/// 地址重建冷却时间戳
static LAST_ADDR_RESTORE: Mutex<Option<Instant>> = Mutex::new(None);

/// 执行 `ip` 命令，返回 stdout；失败返回含 stderr 的错误
fn ip(args: &[&str]) -> Result<String, String> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run ip {:?}: {}", args, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("ip {:?} failed: {}", args, stderr.trim()));
    }
    Ok(stdout)
}

/// 删除路由（不存在时静默忽略）
fn route_del(dest: &str, dev: &str, table: Option<&str>) {
    let mut args = vec!["-6", "route", "del", dest, "dev", dev];
    if let Some(t) = table {
        args.extend_from_slice(&["table", t]);
    }
    let _ = ip(&args);
}

/// 添加路由（已存在时忽略，保持幂等）
fn route_add(dest: &str, dev: &str, table: Option<&str>, metric: Option<&str>) {
    let mut args = vec!["-6", "route", "add", dest, "dev", dev];
    if let Some(m) = metric {
        args.extend_from_slice(&["metric", m]);
    }
    if let Some(t) = table {
        args.extend_from_slice(&["table", t]);
    }
    if let Err(e) = ip(&args) {
        if !e.contains("File exists") {
            debug!(error = %e, dest, table, "route add skipped");
        }
    }
}

/// 写 sysctl（/proc 文件：只写、截断，不创建）
fn write_sysctl(path: &str, value: &str) -> bool {
    use std::io::Write;
    if !std::path::Path::new(path).exists() {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .and_then(|mut f| f.write_all(value.as_bytes()))
        .is_ok()
}

/// 确保 IPv6 转发开启（all / usb0 / sipa_eth0）
fn ensure_forwarding() -> bool {
    let mut acted = false;
    for path in [
        "/proc/sys/net/ipv6/conf/all/forwarding",
        "/proc/sys/net/ipv6/conf/usb0/forwarding",
        "/proc/sys/net/ipv6/conf/sipa_eth0/forwarding",
    ] {
        let cur = std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if cur != "1" && write_sysctl(path, "1") {
            info!(path, "enabled IPv6 forwarding");
            acted = true;
        }
    }
    acted
}

/// 解析 `ip -6 addr show dev X` 输出中的全局地址
///
/// 返回 (完整地址, 前缀长度, /64 前缀字符串)
fn parse_global_addrs(output: &str) -> Vec<(String, String, Option<String>)> {
    let mut result = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with("inet6 ") || !line.contains("scope global") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _ = parts.next(); // "inet6"
        let Some(addr_plen) = parts.next() else { continue };
        if addr_plen.starts_with("fe80") || addr_plen.starts_with("::") {
            continue; // 链路本地 / 环回
        }
        let (addr, plen) = match addr_plen.split_once('/') {
            Some((a, p)) => (a.to_string(), p.to_string()),
            None => (addr_plen.to_string(), "128".to_string()),
        };
        // /64 前缀 = 前四组（兼容 "a:b:c:d::1" 压缩形式）
        let groups: Vec<&str> = addr.split(':').collect();
        let prefix64 = if groups.len() >= 4 {
            Some(format!("{}:{}:{}:{}", groups[0], groups[1], groups[2], groups[3]))
        } else {
            None
        };
        result.push((addr, plen, prefix64));
    }
    result
}

/// 取蜂窝接口当前 GUA 的 /64 前缀
fn wan_prefix64() -> Option<String> {
    let out = ip(&["-6", "addr", "show", "dev", WAN_IF]).ok()?;
    parse_global_addrs(&out).into_iter().find_map(|(_, _, p)| p)
}

/// usb0 当前全局地址列表
fn usb0_globals() -> Vec<(String, String, Option<String>)> {
    ip(&["-6", "addr", "show", "dev", USB_IF])
        .map(|o| parse_global_addrs(&o))
        .unwrap_or_default()
}

/// 推导 usb0 的稳定 IID（后四组）
///
/// 优先复用既有全局地址的 IID（与 connman 派生方式保持一致），
/// 其次按 usb0 MAC 的 EUI-64 生成。
fn stable_iid() -> Option<String> {
    for (addr, _, _) in usb0_globals() {
        let groups: Vec<&str> = addr.split(':').collect();
        if groups.len() == 8 {
            return Some(format!(
                "{}:{}:{}:{}",
                groups[4], groups[5], groups[6], groups[7]
            ));
        }
    }
    let mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", USB_IF)).ok()?;
    let mac = mac.trim().to_string();
    let p: Vec<&str> = mac.split(':').collect();
    if p.len() != 6 {
        return None;
    }
    let m0 = u8::from_str_radix(p[0], 16).ok()? ^ 0x02;
    // cc:e8:ac:c0:00:00 → cee8:acff:fec0:0000
    Some(format!("{:02x}{}:{}ff:fe{}:{}{}", m0, p[1], p[2], p[3], p[4], p[5]))
}

/// 地址卫生：**先补后清**（前缀切换时不留无地址空窗）
///
/// - 当前前缀地址缺失 → 先 add（受 120s 冷却保护，冷却期内保留旧地址兜底）
/// - 当前地址就位后 → 删除所有陈旧前缀地址
/// 返回删除数量
fn sync_usb0_addresses(cur_prefix: &str, iid: &str) -> usize {
    let globals = usb0_globals();
    let has_current = globals.iter().any(|(_, _, p)| p.as_deref() == Some(cur_prefix));

    if !has_current {
        // 冷却期内不重建（旧地址暂留作兜底，等冷却结束）
        let in_cooldown = LAST_ADDR_RESTORE
            .lock()
            .ok()
            .and_then(|t| *t)
            .map(|t| t.elapsed() < ADDR_RESTORE_COOLDOWN)
            .unwrap_or(false);
        if in_cooldown {
            debug!("addr restore in cooldown, keeping stale addr as fallback");
            return 0;
        }

        let desired = format!("{}:{}/128", cur_prefix, iid);
        let added = match ip(&["-6", "addr", "add", &desired, "dev", USB_IF]) {
            Ok(_) => {
                info!(addr = %desired, "added current IPv6 address to usb0");
                true
            }
            Err(e) if e.contains("File exists") => true,
            Err(e) => {
                warn!(error = %e, addr = %desired, "failed to add current IPv6 address");
                false
            }
        };

        // 无论成败都记入冷却，避免对失败的 add 每周期重试
        if let Ok(mut ts) = LAST_ADDR_RESTORE.lock() {
            *ts = Some(Instant::now());
        }
        if !added {
            return 0; // 补不上就不清旧的，保留兜底地址
        }
    }

    // 先补后清：当前地址已就位，清理陈旧前缀
    let mut removed = 0;
    for (addr, plen, prefix) in &globals {
        if prefix.as_deref() != Some(cur_prefix) {
            if ip(&["-6", "addr", "del", &format!("{}/{}", addr, plen), "dev", USB_IF]).is_ok() {
                removed += 1;
                info!(addr = %addr, "removed stale IPv6 address on usb0");
            }
        }
    }
    removed
}

/// 读取路由行
///
/// - `None` → main 表（`ip -6 route show`）
/// - `Some(t)` → 用 `table all` 过滤出表 t 的行
///   （本设备 busybox 的 `ip -6 route show table <N>` 常返回空，是显示 bug）
fn route_lines(table: Option<&str>) -> Vec<String> {
    match table {
        None => ip(&["-6", "route", "show"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect(),
        Some(t) => {
            let needle_a = format!(" table {} ", t);
            let needle_b = format!(" table {}", t);
            ip(&["-6", "route", "show", "table", "all"])
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains(&needle_a) || l.ends_with(&needle_b))
                .map(String::from)
                .collect()
        }
    }
}

/// 删除指定表中以 `prefix` 开头、出口为 `dev` 的全部路由
fn flush_prefix_routes(table: Option<&str>, prefix: &str, dev: &str) {
    for line in route_lines(table) {
        if line.starts_with(prefix) && line.contains(&format!("dev {}", dev)) {
            if let Some(dest) = line.split_whitespace().next() {
                route_del(dest, dev, table);
            }
        }
    }
}

/// 路由修复（仅在回程探测失败或发生过地址变更时执行，避免与 connman 对抗）：
/// - 当前 /64 → usb0：main/181/200 表全删再加（真去重）
/// - 陈旧前缀 → usb0 路由：全部删除
/// - 181 表回程 default：缺失则补
fn sync_routes(cur_prefix: &str, stale_prefixes: &[String]) {
    let cur_route = format!("{}/64", cur_prefix);

    let mut tables: Vec<Option<&str>> = vec![None]; // main
    tables.extend(EXTRA_TABLES.iter().map(|t| Some(*t)));
    tables.push(Some(TETHER_TABLE));

    for table in &tables {
        flush_prefix_routes(*table, &cur_route, USB_IF);
        route_add(&cur_route, USB_IF, *table, Some("1024"));
    }

    // 陈旧前缀的残留路由
    for stale in stale_prefixes {
        let stale_route = format!("{}/64", stale);
        if stale_route == cur_route {
            continue;
        }
        for table in &tables {
            flush_prefix_routes(*table, &stale_route, USB_IF);
        }
    }

    // 181 表回程 default（蜂窝出口）缺失则补
    let has_181_default = route_lines(Some(TETHER_TABLE))
        .iter()
        .any(|l| l.starts_with("default") && l.contains(WAN_IF));
    if !has_181_default {
        route_add("default", WAN_IF, Some(TETHER_TABLE), Some("1024"));
        info!("restored default route in tether table 181");
    }
}

/// 策略规则保障（3 条，priority 与 connman 一致）：
/// - `iif sipa_eth0 → lookup 181` prio 422（蜂窝入口回程）
/// - `iif usb0     → lookup 181` prio 442（PC 入口出网）
/// - `lookup 200` prio 2000（connman 前缀表）
fn sync_rules() {
    let out = ip(&["-6", "rule", "show"]).unwrap_or_default();

    // iif → 181（存在性检查与 priority 无关）
    for (iface, prio) in [(WAN_IF, RULE_PRIO_IIF_WAN), (USB_IF, RULE_PRIO_IIF_USB)] {
        if !out.contains(&format!("iif {} lookup {}", iface, TETHER_TABLE)) {
            if ip(&[
                "-6", "rule", "add", "iif", iface, "lookup", TETHER_TABLE, "priority", prio,
            ])
            .is_ok()
            {
                info!(iface, table = TETHER_TABLE, prio, "restored IPv6 policy rule");
            }
        }
    }

    // lookup 200（按行尾字段判断，避免被 "lookup 2000" 误匹配）
    let has_200 = out
        .lines()
        .any(|l| l.split_whitespace().last() == Some(CONNMAN_TABLE));
    if !has_200 {
        if ip(&[
            "-6", "rule", "add", "lookup", CONNMAN_TABLE, "priority", RULE_PRIO_LOOKUP_200,
        ])
        .is_ok()
        {
            info!(table = CONNMAN_TABLE, "restored IPv6 policy rule");
        }
    }
}

/// 回程探测：模拟"蜂窝入口收到发往 PC 侧地址的回包"
///
/// 带 `from` 伪源贴近真实回程流量；期望命中 `dev usb0` + `table 181`。
fn probe_return_path(cur_prefix: &str) -> Result<(), String> {
    let target = format!("{}::2", cur_prefix);
    let out = ip(&[
        "-6", "route", "get", &target, "from", PROBE_SRC, "iif", WAN_IF,
    ])?;

    if !out.contains(&format!("dev {}", USB_IF)) {
        return Err(format!("return path does not egress {}: {}", USB_IF, out.trim()));
    }
    if !out.contains(&format!("table {}", TETHER_TABLE)) {
        return Err(format!("return path misses table {}: {}", TETHER_TABLE, out.trim()));
    }
    Ok(())
}

/// 设备自身 IPv6 出口探测
///
/// 期望本机发往蜂窝可达目标的流量走 `dev sipa_eth0`；
/// 若路由结果被 `dev usb0` 劫持（如被塞入 `default dev usb0`），说明出口断裂。
fn probe_device_egress() -> Result<String, String> {
    let out = ip(&["-6", "route", "get", EGRESS_PROBE_TARGET])?;
    if out.contains(&format!("dev {}", USB_IF)) {
        return Err(format!("device egress hijacked by {}: {}", USB_IF, out.trim()));
    }
    Ok(out)
}

/// 修复设备出口被 usb0 default 劫持：删除 main 与 table200 中的 `default dev usb0`
fn repair_device_egress() {
    for table in [None, Some(CONNMAN_TABLE)] {
        for line in route_lines(table) {
            let t: Vec<&str> = line.split_whitespace().collect();
            // "default dev usb0 ..."（带不带 metric 均匹配）
            if t.len() >= 3 && t[0] == "default" && t[1] == "dev" && t[2] == USB_IF {
                route_del("default", USB_IF, table);
                info!(table = table.unwrap_or("main"), "removed hijacking default via usb0");
            }
        }
    }
}

/// 单轮自愈；返回 Some(动作摘要) 表示执行了修复，None 表示健康
pub fn heal_once() -> Option<String> {
    if !std::path::Path::new(&format!("/sys/class/net/{}", USB_IF)).exists() {
        return None;
    }
    let cur_prefix = wan_prefix64()?;
    let Some(iid) = stable_iid() else {
        warn!("ipv6 heal: cannot derive stable IID, skipped");
        return None;
    };

    let mut actions: Vec<String> = Vec::new();

    // 1) forwarding 确保
    if ensure_forwarding() {
        actions.push("enabled ipv6 forwarding".to_string());
    }

    // 2) 采集陈旧前缀（须在删地址之前）
    let mut stale: Vec<String> = usb0_globals()
        .into_iter()
        .filter_map(|(_, _, p)| p)
        .filter(|p| p != &cur_prefix)
        .collect();
    stale.sort();
    stale.dedup();

    // 3) 地址卫生（先补后清 + 冷却）
    let removed = sync_usb0_addresses(&cur_prefix, &iid);
    if removed > 0 {
        actions.push(format!("removed {} stale addr", removed));
    }

    // 4) 规则保障（先于探测，探测即验证修复后的状态）
    sync_rules();

    // 5) 回程探测：失败（或刚发生过地址变更）才重建路由，避免与 connman 对抗
    match probe_return_path(&cur_prefix) {
        Ok(()) => {
            if removed > 0 {
                sync_routes(&cur_prefix, &stale);
                actions.push("rebuilt routes after addr change".to_string());
            }
        }
        Err(e) => {
            actions.push(format!("probe failed: {}", e));
            sync_routes(&cur_prefix, &stale);
            match probe_return_path(&cur_prefix) {
                Ok(()) => actions.push("probe recovered".to_string()),
                Err(e2) => actions.push(format!("probe still failing: {}", e2)),
            }
        }
    }

    // 6) 设备自身出口探测（防 default dev usb0 劫持）
    match probe_device_egress() {
        Ok(_) => {}
        Err(e) => {
            actions.push(format!("egress probe: {}", e));
            repair_device_egress();
            if probe_device_egress().is_ok() {
                actions.push("egress recovered".to_string());
            }
        }
    }

    if actions.is_empty() {
        debug!(prefix = %cur_prefix, "ipv6 share healthy");
        None
    } else {
        Some(actions.join("; "))
    }
}

/// 启动自愈后台任务
///
/// - 启动 15 秒后执行首轮（清理历史堆积）
/// - 之后每 30 秒：探测驱动、幂等修复
pub fn spawn_ipv6_share_heal() {
    tokio::spawn(async {
        info!(interval = HEAL_INTERVAL_SECS, "IPv6 share self-heal task started");
        tokio::time::sleep(tokio::time::Duration::from_secs(HEAL_INITIAL_DELAY_SECS)).await;
        loop {
            match tokio::task::spawn_blocking(heal_once).await {
                Ok(Some(action)) => info!(action = %action, "IPv6 share healed"),
                Ok(None) => debug!("IPv6 share check ok"),
                Err(e) => warn!(error = %e, "IPv6 share heal task crashed"),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(HEAL_INTERVAL_SECS)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_global_addrs() {
        let sample = "    inet6 2408:845d:7d12:df05::1/64 scope global\n    inet6 fe80::1/64 scope link \n    inet6 2408:845d:7d01:99fc:cee8:acff:fec0:0/128 scope global \n";
        let addrs = parse_global_addrs(sample);
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0].2.as_deref(), Some("2408:845d:7d12:df05"));
        assert_eq!(addrs[1].0, "2408:845d:7d01:99fc:cee8:acff:fec0:0");
        assert_eq!(addrs[1].2.as_deref(), Some("2408:845d:7d01:99fc"));
    }

    #[test]
    fn test_parse_skips_link_local() {
        let sample = "    inet6 fe80::cee8:acff:fec0:0/64 scope link \n    inet6 ::1/128 scope host \n";
        assert!(parse_global_addrs(sample).is_empty());
    }
}
