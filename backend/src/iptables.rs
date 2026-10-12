/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-07 07:33:11
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:46:06
 * @FilePath: /udx710-backend/backend/src/iptables.rs
 * @Description: 
 * 
 * Copyright (c) 2025 by 1orz, All Rights Reserved. 
 */
//! iptables 操作模块
//!
//! 提供 iptables 规则检查和清空功能

use std::process::Command;
use tokio::task;

/// 需要定点清除的转发 DROP 规则所涉及的接口
const USB_IF: &str = "usb0";
const WAN_IF: &str = "sipa_eth0";

/// 定点删除 `usb0 → sipa_eth0` 的 FORWARD DROP 规则（v4 + v6，幂等）
///
/// 厂商开机脚本 `/etc/route_test.sh` 会插入该规则，直接掐断
/// "电脑/OpenWrt(usb0) → 设备 → 蜂窝(sipa_eth0)" 的共享转发。
///
/// ## 设计约束（重要）
/// - **只删这一条规则**，绝不 `iptables -F` 整表清空：整表 flush 会误杀
///   connman/厂商的其他合法规则，且与 connman 形成对抗循环
///   （数据看门狗旧实现的教训）
/// - 幂等：规则不存在时 `-C` 返回非零，直接跳过
/// - 每轮最多删除3 条（防止异常场景下无限循环）
///
/// 供 `tether_health` 巡检（同步上下文）调用。
///
/// # Returns
/// * `true` - 本次删除了至少一条规则
pub fn clear_usb0_drop_rules_sync() -> bool {
    let mut removed = false;
    for cmd in ["iptables", "ip6tables"] {
        for _ in 0..3 {
            let check = Command::new(cmd)
                .args(["-C", "FORWARD", "-i", USB_IF, "-o", WAN_IF, "-j", "DROP"])
                .output();
            let exists = check.map(|o| o.status.success()).unwrap_or(false);
            if !exists {
                break;
            }
            let deleted = Command::new(cmd)
                .args(["-D", "FORWARD", "-i", USB_IF, "-o", WAN_IF, "-j", "DROP"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !deleted {
                break;
            }
            removed = true;
        }
    }
    removed
}

/// iptables 规则统计信息
#[derive(Debug, Default)]
pub struct IptablesRuleCount {
    pub ipv4_rules: usize,
    pub ipv6_rules: usize,
}

#[allow(dead_code)] // 保留给手动"重置防火墙"场景的统计接口
impl IptablesRuleCount {
    /// 是否有任何规则
    pub fn has_rules(&self) -> bool {
        self.ipv4_rules > 0 || self.ipv6_rules > 0
    }
    
    /// 总规则数
    pub fn total(&self) -> usize {
        self.ipv4_rules + self.ipv6_rules
    }
}

/// 获取 iptables 规则数量
///
/// 统计 iptables 和 ip6tables 中 filter 表的规则数量（排除默认策略行）
///
/// # Returns
/// * `Ok(IptablesRuleCount)` - 规则统计
/// * `Err(String)` - 操作失败的错误信息
#[allow(dead_code)] // 保留给手动"重置防火墙"场景；自动路径已改为定点删除
pub async fn get_iptables_rule_count() -> Result<IptablesRuleCount, String> {
    task::spawn_blocking(|| {
        let mut count = IptablesRuleCount::default();
        
        // 获取 iptables 规则数量
        // iptables -L -n 输出中，每条规则是一行，但需要排除链名行和策略行
        // 使用 iptables -S 更简单，每条规则一行，-P 开头的是策略，-A 开头的是规则
        if let Ok(output) = Command::new("iptables").args(["-S"]).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // 统计 -A 开头的行（实际规则），排除 -P（策略）和 -N（链定义）
                count.ipv4_rules = stdout.lines()
                    .filter(|line| line.starts_with("-A "))
                    .count();
            }
        }
        
        // 获取 ip6tables 规则数量
        if let Ok(output) = Command::new("ip6tables").args(["-S"]).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                count.ipv6_rules = stdout.lines()
                    .filter(|line| line.starts_with("-A "))
                    .count();
            }
        }
        
        Ok(count)
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

/// 清空所有 iptables 规则
///
/// 执行等同于 `iptables -F` 的操作，清空 filter 表的所有链
///
/// # Returns
/// * `Ok(())` - 成功清空规则
/// * `Err(String)` - 操作失败的错误信息
///
/// # 说明
/// 此函数会清空以下链的规则：
/// - INPUT 链
/// - FORWARD 链
/// - OUTPUT 链
#[allow(dead_code)] // 保留给手动"重置防火墙"场景；自动路径禁止整表 flush
pub async fn flush_iptables() -> Result<(), String> {
    task::spawn_blocking(|| {
        // 清空 filter 表的所有规则
        let outputv4 = Command::new("iptables")
            .arg("-F")
            .output()
            .map_err(|e| format!("Failed to execute ip6tables: {}", e))?;
        if !outputv4.status.success()  {
            let stderr = String::from_utf8_lossy(&outputv4.stderr);
            return Err(format!("iptables -F failed: {}", stderr));
        }
        let outputv6 = Command::new("ip6tables")
            .arg("-F")
            .output()
            .map_err(|e| format!("Failed to execute ip6tables: {}", e))?;
        if !outputv6.status.success()  {
            let stderr = String::from_utf8_lossy(&outputv6.stderr);
            return Err(format!("ip6tables -F failed: {}", stderr));
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

/// 清空所有 iptables 规则（包括 nat 和 mangle 表）
///
/// 执行更完整的清空操作，清空 filter、nat、mangle 表的所有规则
///
/// # Returns
/// * `Ok(())` - 成功清空规则
/// * `Err(String)` - 操作失败的错误信息
#[allow(dead_code)]
pub async fn flush_all_iptables() -> Result<(), String> {
    task::spawn_blocking(|| {
        let tables = ["filter", "nat", "mangle"];
        
        for table in &tables {
            let output = Command::new("iptables")
                .arg("-t")
                .arg(table)
                .arg("-F")
                .output()
                .map_err(|e| format!("Failed to execute iptables for table {}: {}", table, e))?;

            if !output.status.success() {
                // 如果表不存在或不支持，继续处理下一个表（某些表可能不存在）
                // 静默处理，不输出警告
            }
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // 需要 root 权限，默认忽略
    async fn test_flush_iptables() {
        let result = flush_iptables().await;
        assert!(result.is_ok());
    }
}

