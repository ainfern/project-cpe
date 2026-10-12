/*
 * @FilePath: /udx710-backend/backend/src/tether_health.rs
 *
 * Copyright (c) 2026 by 1orz, All Rights Reserved.
 */
//! USB 共享健康巡检（点1/2：SFP、usb0 守护、开机脚本持久修补、NAT 保障）
//!
//! ## 拓扑（重要）
//! ```text
//! PC --WiFi--> OpenWrt 路由器 --USB(RNDIS/ECM)--> 本设备 usb0(192.168.66.1)
//!                                                    └-- NAT --> 蜂窝 sipa_eth0
//! ```
//! USB 对端（192.168.66.2）是 **OpenWrt 路由器**而非单台 PC，因此
//! 66 网段 / NAT / 错误别名(67.1) 任何异常都会切断**整条路由链路**
//! （路由器下所有 WiFi 客户端一起断），严重性高于单机故障。
//!
//! ## 巡检项（全部幂等：缺了才补、多了才删）
//! 1. 关闭 SFP 硬件转发（USB+IPv6 场景 `sfp init mac fail` → usb0 RX 错误飙升 → USB 断连）
//! 2. 关闭冲突接口 sipa_usb0
//! 3. 守护 usb0 地址 192.168.66.1/24（丢失即补，**绝不 down/up 复位接口**）
//! 4. 清除 connman 偶发的 192.168.67.1 错误别名（会让对端丢网段）
//! 5. 定点删除 FORWARD DROP（复用 iptables 模块，绝不整表 -F）
//! 6. 保障 NAT MASQUERADE（66/24 → 蜂窝），不依赖 connmanctl tether
//! 7. 持久修补 /etc/route_test.sh（消灭开机自动打开 SFP / 插入 DROP 的毒源）
//! 8. 守护 rp_filter=0（all/default/usb0/sipa_eth0；严格反向路径过滤会丢非对称包）
//!
//! ## 设计红线
//! - 禁止 `iptables -F` / `ip6tables -F`（整表清空误杀合法规则）
//! - 禁止自动 soft-reset / UDC bounce / connmanctl tether off→on（会弄僵 RNDIS 或改网段）
//! - 所有动作幂等、可在任意周期重复执行
//!
//! ## 已知噪声：usb0 `RX errors/frame` 是驱动统计，不是坏包
//! `sipa_usb` 路径给**每个 RX 包 1:1 记一次 `rx_frame_errors`**（errs 恒 = pkts + 120，
//! 那 120 是开机 bring-up 一次性产生）。实测依据（2026-10-12）：6MB adb push（其间
//! +6216 "errors"）md5 一致；`InCsumErrors=0`、`rx_dropped=0`、`overruns=0`；
//! TCP 重传 313/389214 ≈ 0.08%；dmesg 长时间无 USB reset/disconnect。
//! **判断链路健康看 `rx_dropped` / `RetransSegs` / `InCsumErrors`，别看 errors/frame**
//! （曾被误读成 SFP 故障复发；userspace 无法修，厂商驱动 stat 映射问题）。

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use tracing::{info, warn};

use crate::iptables::clear_usb0_drop_rules_sync;

/// PC/路由器侧共享接口
const USB_IF: &str = "usb0";
/// 蜂窝上行接口
const WAN_IF: &str = "sipa_eth0";

/// 默认 USB 网络接口 IP（须与 usb_switch 保持一致）
const USB_INTERFACE_IP: &str = "192.168.66.1";
/// connman tether 重启后偶发的错误别名
const USB_BAD_ALIAS_IP: &str = "192.168.67.1";

/// SFP（Sprd Forward Path）硬件转发开关
const SFP_ENABLE_PATH: &str = "/proc/net/sfp/enable";
const SFP_TETHER_SCHEME_PATH: &str = "/proc/net/sfp/tether_scheme";

/// 厂商开机初始化脚本（含打开 SFP 与插入 FORWARD DROP 的毒源）
const ROUTE_TEST_SCRIPT_PATH: &str = "/etc/route_test.sh";

/// 需要保证 rp_filter=0 的网络配置（`conf/all` 与各接口分别守护）
///
/// rp_filter 非 0 时内核按反向路径校验丢包，非对称/转发路径上表现为
/// 偶发丢包且极难排查；正常值 0，被内核/connman/脚本改回非 0 时这里纠正。
const RP_FILTER_CONFS: [&str; 4] = ["all", "default", "usb0", "sipa_eth0"];

fn write_file(path: &str, content: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// 立即关闭 SFP（同步，供 USB 模式切换等事件点调用）
///
/// SFP 在 USB 共享 + IPv6 场景下会因 `sfp init mac fail` 导致
/// usb0 RX frame 错误飙升、最终 USB 断连。关闭后走内核软件转发。
pub fn disable_sfp_now() {
    for path in [SFP_ENABLE_PATH, SFP_TETHER_SCHEME_PATH] {
        if Path::new(path).exists() {
            let _ = write_file(path, "0");
        }
    }
}

fn sfp_is_enabled(path: &str) -> bool {
    fs::read_to_string(path)
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// 若 SFP 被系统（如 connman 开 tether 时）重新打开则再次关闭
fn ensure_sfp_disabled() -> bool {
    let needs = (Path::new(SFP_ENABLE_PATH).exists() && sfp_is_enabled(SFP_ENABLE_PATH))
        || (Path::new(SFP_TETHER_SCHEME_PATH).exists() && sfp_is_enabled(SFP_TETHER_SCHEME_PATH));
    if needs {
        disable_sfp_now();
        return true;
    }
    false
}

fn iface_has_ip(iface: &str, ip: &str) -> bool {
    Command::new("ifconfig")
        .arg(iface)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|t| t.contains(ip))
        .unwrap_or(false)
}

/// 关闭与 usb0 冲突的 IPA USB 接口
fn ensure_sipa_usb0_down() -> bool {
    let up = Command::new("ifconfig")
        .arg("sipa_usb0")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|t| t.contains("UP"))
        .unwrap_or(false);
    if !up {
        return false;
    }
    let _ = Command::new("ifconfig").args(["sipa_usb0", "down"]).output();
    true
}

/// 守护 usb0 的 IPv4 地址（丢失即补；接口异常只补地址，绝不复位接口）
fn ensure_usb0_address() -> bool {
    if iface_has_ip(USB_IF, USB_INTERFACE_IP) {
        return false;
    }
    let _ = Command::new("ifconfig")
        .args([USB_IF, USB_INTERFACE_IP, "netmask", "255.255.255.0", "up"])
        .output();
    true
}

/// 清除 connman 偶发挂上的 192.168.67.1/24 错误别名（对端会因此丢66 网段）
fn clear_usb0_bad_alias() -> bool {
    if !iface_has_ip(USB_IF, USB_BAD_ALIAS_IP) {
        return false;
    }
    Command::new("ip")
        .args([
            "addr",
            "del",
            &format!("{}/24", USB_BAD_ALIAS_IP),
            "dev",
            USB_IF,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 守护 rp_filter=0（幂等：读到非 0 才写回，接口不存在则跳过）
///
/// `conf/all` 与接口值取 max 生效，两者都要是 0 才真正关闭反向路径过滤。
fn ensure_rp_filter_disabled() -> bool {
    let mut acted = false;
    for conf in RP_FILTER_CONFS {
        let path = format!("/proc/sys/net/ipv4/conf/{}/rp_filter", conf);
        let current = match fs::read_to_string(&path) {
            Ok(v) => v,
            // 接口尚未创建（如 usb0 未枚举）时该目录不存在，下一轮自然会补
            Err(_) => continue,
        };
        if current.trim() == "0" {
            continue;
        }
        match write_file(&path, "0") {
            Ok(()) => {
                acted = true;
                info!(conf, value = %current.trim(), "tether: rp_filter reset to 0");
            }
            Err(e) => warn!(conf, error = %e, "tether: failed to write rp_filter"),
        }
    }
    acted
}

/// 保障 `192.168.66.0/24 → 蜂窝` 的 NAT MASQUERADE
///
/// 只做增量补齐（`-C` 检查），不调用 connmanctl tether
/// （实测它会改网段、权限失败、甚至弄坏 USB 枚举）。
fn ensure_usb66_masquerade() -> bool {
    let check = Command::new("iptables")
        .args([
            "-t", "nat", "-C", "POSTROUTING",
            "-s", "192.168.66.0/24", "-o", WAN_IF, "-j", "MASQUERADE",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if check {
        return false;
    }
    // 优先挂进 connman 链（若存在），否则挂主链
    let via_connman = Command::new("iptables")
        .args([
            "-t", "nat", "-A", "connman-POSTROUTING",
            "-s", "192.168.66.0/24", "-o", WAN_IF, "-j", "MASQUERADE",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if via_connman {
        return true;
    }
    Command::new("iptables")
        .args([
            "-t", "nat", "-A", "POSTROUTING",
            "-s", "192.168.66.0/24", "-o", WAN_IF, "-j", "MASQUERADE",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 以可写方式写根分区文件（必要时临时 remount rw，写完恢复 ro）
fn write_rootfs_file(path: &Path, content: &str) -> io::Result<()> {
    if fs::write(path, content).is_ok() {
        return Ok(());
    }
    let _ = Command::new("mount").args(["-o", "remount,rw", "/"]).output();
    let result = fs::write(path, content);
    let _ = Command::new("mount").args(["-o", "remount,ro", "/"]).output();
    result
}

/// 持久修补 `/etc/route_test.sh`：开机不再打开 SFP，并注释掉会掐断
/// USB 共享的 FORWARD DROP 规则。
///
/// 原脚本每次开机执行：
/// - `echo 1 > /proc/net/sfp/enable` / `tether_scheme`（重新打开 SFP）
/// - `iptables/ip6tables -I FORWARD -i usb0 -o sipa_eth0 -j DROP`（掐断共享）
///
/// 不修这个，运行时巡检只能在"开机 → backend 首轮巡检"之间留下故障窗口；
/// 修一次则重启免疫（OTA 只替换 /home/root，不会覆盖 /etc）。
///
/// 幂等：内容无变化时直接返回 false；若脚本改版导致模式匹配不上则告警。
pub fn patch_route_test_disable_sfp() -> io::Result<bool> {
    let path = Path::new(ROUTE_TEST_SCRIPT_PATH);
    if !path.exists() {
        return Ok(false);
    }

    let original = fs::read_to_string(path)?;
    let mut patched = original
        .replace(
            "echo 1 > /proc/net/sfp/enable",
            "echo 0 > /proc/net/sfp/enable",
        )
        .replace(
            "echo 1 > /proc/net/sfp/tether_scheme",
            "echo 0 > /proc/net/sfp/tether_scheme",
        );

    patched = patched
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let should_comment = !trimmed.starts_with('#')
                && (trimmed.starts_with("iptables -I FORWARD -i usb0")
                    || trimmed.starts_with("ip6tables -I FORWARD -i usb0"));
            if should_comment {
                format!("# {}", line)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if !patched.ends_with('\n') {
        patched.push('\n');
    }

    if patched == original {
        // 已是补丁后状态；若仍存在未匹配的毒源变体，告警提示人工检查
        if original.contains("/proc/net/sfp") && original.contains("echo 1 > /proc/net/sfp") {
            warn!(
                path = ROUTE_TEST_SCRIPT_PATH,
                "route_test.sh still contains unmatched 'echo 1 > /proc/net/sfp' variant"
            );
        }
        return Ok(false);
    }

    write_rootfs_file(path, &patched)?;
    info!(path = ROUTE_TEST_SCRIPT_PATH, "patched route_test.sh (SFP off, DROP commented)");
    Ok(true)
}

/// 同步巡检核心（在 blocking 上下文执行）
fn ensure_health_sync() -> Result<bool, String> {
    let mut acted = false;

    if ensure_sfp_disabled() {
        acted = true;
        info!("tether: SFP was re-enabled, disabled again");
    }
    if ensure_sipa_usb0_down() {
        acted = true;
        info!("tether: brought conflicting sipa_usb0 down");
    }
    if ensure_usb0_address() {
        acted = true;
        info!(ip = USB_INTERFACE_IP, "tether: restored usb0 address");
    }
    if clear_usb0_bad_alias() {
        acted = true;
        info!(ip = USB_BAD_ALIAS_IP, "tether: removed bad alias on usb0");
    }
    if clear_usb0_drop_rules_sync() {
        acted = true;
        info!("tether: removed usb0→sipa FORWARD DROP");
    }
    if ensure_usb66_masquerade() {
        acted = true;
        info!("tether: restored NAT MASQUERADE for 192.168.66.0/24");
    }
    if ensure_rp_filter_disabled() {
        // 明细（哪个 conf、原值）已在函数内按项打印
        acted = true;
    }
    if let Err(e) = patch_route_test_disable_sfp() {
        // 修补失败不阻断其他巡检项
        warn!(error = %e, "tether: failed to patch route_test.sh");
    }

    Ok(acted)
}

/// USB 共享健康巡检（幂等）
///
/// # Returns
/// * `Ok(true)` - 本轮执行了至少一次修正
/// * `Ok(false)` - 状态健康，无动作
pub async fn ensure_usb_tether_health() -> Result<bool, String> {
    tokio::task::spawn_blocking(ensure_health_sync)
        .await
        .map_err(|e| format!("health task join error: {}", e))?
}
