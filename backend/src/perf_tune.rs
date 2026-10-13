/*
 * @FilePath: /udx710-backend/backend/src/perf_tune.rs
 *
 * Copyright (c) 2026 by 1orz, All Rights Reserved.
 */
//! IRQ affinity + RPS 性能调优（UDX710 平台，可选开关）
//!
//! 5G 收包 IRQ（`sprd,sipa`）与 USB 出口 IRQ（`xhci-hcd`）默认落在 CPU0，
//! 高频中断单核串行会压低下行上限。本模块把 xhci 挪到 CPU1，并开 RPS
//! 让 RX 软中断分散到双核。
//!
//! 设计边界：
//! - **默认关闭**，由系统配置面板开关控制（`config.json` 持久化）
//! - 幂等：只在偏离目标值时写入；无进程会主动覆盖这些节点
//! - 触发点：启动、开关变更、以及既有 USB 健康巡检顺带重套
//!   （`usb0`/`sipa_eth0` 重建会把 `rps_cpus` 重置为 0，须在接口回来后补写）
//! - 不做独立周期巡检；收益依赖空口条件，不作吞吐承诺

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};

/// 用户开关镜像（config.json 为准；巡检路径无 ConfigManager，读这里）
static ENABLED: AtomicBool = AtomicBool::new(false);

/// xhci USB 出口 IRQ（按名字从 /proc/interrupts 解析，找不到再回退平台默认号）
const XHCI_IRQ_NAME: &str = "xhci-hcd";
const XHCI_IRQ_FALLBACK: &str = "97";
/// xhci 只放 CPU1（bitmask 2）
const XHCI_AFFINITY: &str = "2";
/// 默认亲和性（双核）
const DEFAULT_AFFINITY: &str = "3";

/// 需要开 RPS 的网卡（单队列，重建后 rps_cpus 会归零）
const RPS_IFACES: [&str; 2] = ["sipa_eth0", "usb0"];
/// RPS 掩码：CPU0+CPU1
const RPS_MASK: &str = "3";
/// 默认（关闭调优）
const RPS_DEFAULT: &str = "0";
/// RPS flow table
const RPS_FLOW_PATH: &str = "/proc/sys/net/core/rps_sock_flow_entries";
const RPS_FLOW_ON: &str = "32768";
const RPS_FLOW_OFF: &str = "0";

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn write_if_changed(path: &str, want: &str) -> io::Result<bool> {
    if !Path::new(path).exists() {
        return Ok(false);
    }
    if read_trimmed(path).as_deref() == Some(want) {
        return Ok(false);
    }
    fs::write(path, want)?;
    Ok(true)
}

/// 按 IRQ 名解析编号（如 `xhci-hcd` → `97`）
fn find_irq_by_name(substr: &str) -> Option<String> {
    let text = fs::read_to_string("/proc/interrupts").ok()?;
    for line in text.lines() {
        if !line.contains(substr) {
            continue;
        }
        let num = line.split(':').next()?.trim();
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return Some(num.to_string());
        }
    }
    None
}

fn xhci_irq() -> String {
    find_irq_by_name(XHCI_IRQ_NAME).unwrap_or_else(|| XHCI_IRQ_FALLBACK.to_string())
}

fn rps_cpus_path(iface: &str) -> String {
    // 本平台 sipa_eth0/usb0 均为单队列 rx-0
    format!("/sys/class/net/{}/queues/rx-0/rps_cpus", iface)
}

/// 开启调优：xhci→CPU1 + RPS 双核 + flow table 扩容
///
/// # Returns
/// * `Ok(true)` - 本轮修正了至少一项
/// * `Ok(false)` - 已是目标状态
pub fn apply_tuning() -> Result<bool, String> {
    let mut acted = false;
    let irq = xhci_irq();
    let affinity_path = format!("/proc/irq/{}/smp_affinity", irq);

    match write_if_changed(&affinity_path, XHCI_AFFINITY) {
        Ok(true) => {
            acted = true;
            info!(irq = %irq, "perf_tune: xhci IRQ affinity -> CPU1");
        }
        Ok(false) => {}
        Err(e) => {
            warn!(irq = %irq, error = %e, "perf_tune: failed to set IRQ affinity");
            return Err(format!("IRQ affinity: {}", e));
        }
    }

    for iface in RPS_IFACES {
        let path = rps_cpus_path(iface);
        if !Path::new(&path).exists() {
            continue;
        }
        match write_if_changed(&path, RPS_MASK) {
            Ok(true) => {
                acted = true;
                info!(iface, "perf_tune: rps_cpus -> {}", RPS_MASK);
            }
            Ok(false) => {}
            Err(e) => {
                warn!(iface, error = %e, "perf_tune: failed to set rps_cpus");
                return Err(format!("rps_cpus {}: {}", iface, e));
            }
        }
    }

    match write_if_changed(RPS_FLOW_PATH, RPS_FLOW_ON) {
        Ok(true) => {
            acted = true;
            info!("perf_tune: rps_sock_flow_entries -> {}", RPS_FLOW_ON);
        }
        Ok(false) => {}
        Err(e) => {
            warn!(error = %e, "perf_tune: failed to set rps_sock_flow_entries");
            return Err(format!("rps_sock_flow_entries: {}", e));
        }
    }

    Ok(acted)
}

/// 恢复默认：xhci 双核亲和 + RPS 关闭（关闭开关时调用）
pub fn restore_defaults() -> Result<bool, String> {
    let mut acted = false;
    let irq = xhci_irq();
    let affinity_path = format!("/proc/irq/{}/smp_affinity", irq);

    if write_if_changed(&affinity_path, DEFAULT_AFFINITY).unwrap_or(false) {
        acted = true;
        info!(irq = %irq, "perf_tune: IRQ affinity restored to both CPUs");
    }
    for iface in RPS_IFACES {
        let path = rps_cpus_path(iface);
        if write_if_changed(&path, RPS_DEFAULT).unwrap_or(false) {
            acted = true;
            info!(iface, "perf_tune: rps_cpus restored to default");
        }
    }
    if write_if_changed(RPS_FLOW_PATH, RPS_FLOW_OFF).unwrap_or(false) {
        acted = true;
        info!("perf_tune: rps_sock_flow_entries restored to default");
    }

    Ok(acted)
}

/// 按开关状态套用（启动 / 开关变更入口）
pub fn sync_enabled(enabled: bool) -> Result<bool, String> {
    ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        apply_tuning()
    } else {
        // 关闭时不强制还原：仅在曾被改过时才写回默认
        restore_defaults()
    }
}

/// 巡检用：当前开关（由 sync_enabled / set_enabled_flag 维护）
pub fn enabled_flag() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 仅更新开关镜像（启动时从 config 加载、不立即写内核参数）
pub fn set_enabled_flag(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// 巡检顺带重套（仅在开关开启时；幂等）
///
/// 挂在既有 USB 健康巡检上，覆盖 usb0/sipa_eth0 重建后 rps 归零的场景，
/// 不额外起周期任务。
pub fn ensure_if_enabled(enabled: bool) -> Result<bool, String> {
    if !enabled {
        return Ok(false);
    }
    apply_tuning()
}

/// 当前是否处于调优状态（供 API 展示）
pub fn current_state() -> PerfTuneState {
    let irq = xhci_irq();
    let affinity_path = format!("/proc/irq/{}/smp_affinity", irq);
    let affinity = read_trimmed(&affinity_path).unwrap_or_default();
    let mut rps = Vec::new();
    for iface in RPS_IFACES {
        let path = rps_cpus_path(iface);
        if let Some(v) = read_trimmed(&path) {
            rps.push(format!("{}={}", iface, v));
        }
    }
    let flow = read_trimmed(RPS_FLOW_PATH).unwrap_or_default();
    let applied = affinity == XHCI_AFFINITY
        && rps.iter().all(|s| s.ends_with(&format!("={}", RPS_MASK)))
        && flow == RPS_FLOW_ON;
    PerfTuneState {
        irq,
        affinity,
        rps,
        flow,
        applied,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PerfTuneState {
    pub irq: String,
    pub affinity: String,
    pub rps: Vec<String>,
    pub flow: String,
    pub applied: bool,
}
