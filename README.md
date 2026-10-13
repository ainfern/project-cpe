# UDX710 后台管理系统

这是一个为市面上成品 5G CPE 设备开发的高级后台管理系统项目。本项目旨在为现有的 5G CPE 设备提供更多高级功能和可玩性，让用户能够更好地控制和定制他们的 5G CPE 设备。

基于 Rust + Axum + zbus 的 5G/LTE 调制解调器后端服务，通过 ofono D-Bus 接口控制。

powered by Cursor Claude Opus 4.5 & Sonnet 4.5 & OpenAI GPT-5.1/5.2

欢迎pr 和 issue 看到后会尽快处理。

## 免责声明

本项目仅供技术交流和学习使用，不得用于任何非法用途。任何使用本项目造成的任何后果，均与本项目无关，由使用者自行承担。

且目前测试通过的设备仅有：

- 华为5G 通讯壳 P50 P60 Mate系列

其余设备由于缺少设备，本人未做测试，你如果手里有多余的设备，可尝试*小心的*尝试使用，但不提供任何 担保或保证。对设备的造成任何的损坏 本人不承担任何责任。

或者愿意捐献设备来测试，可联系我，我将在第一时间进行测试并更新本项目。

## ⚖️ 开源协议声明

本项目采用 GNU General Public License v3.0 (GPLv3) 开源协议

鉴于目前大部分人对版权意识薄弱，特此声明

本项目采用 GPLv3 开源协议，您可以自由使用、研究、修改本软件，但必须保留所有版权声明和许可证声明，并且公开源代码，任何基于本项目的衍生作品也必须使用 GPLv3 协议。

### ✅ 您可以

- 自由使用、研究、修改本软件
- 分发本软件的副本
- 分发修改后的版本

### ⚠️ 但您必须

1. **保留所有版权声明和许可证声明** - 不得删除或修改原作者的版权信息
2. **公开源代码** - 如果您分发本软件或其修改版本，必须以 GPLv3 协议公开完整源代码
3. **使用相同协议** - 任何基于本项目的衍生作品也必须使用 GPLv3 协议
4. **标注修改** - 修改后的版本必须明确标注修改内容和修改日期
5. **提供许可证副本** - 分发时必须附带完整的 GPLv3 许可证文本

### ❌ 严禁以下行为

- **禁止闭源商业化**：不得将本项目或其衍生版本闭源后进行商业销售
- **禁止删除版权信息**：不得移除原作者的版权声明
- **禁止更改许可证**：不得将本项目改为其他许可证（如 MIT、Apache 等）
- **禁止专有软件化**：不得将本项目整合到专有/闭源软件中而不开源

## 🚀 快速开始

### 构建后端

```bash
# 交叉编译 (Linux -> Linux aarch64)
./scripts/build.sh

# 带 UPX 压缩
./scripts/build.sh --upx
```

### 构建前端

```bash
cd frontend && npm run build
```

### 部署

```bash
./scripts/deploy.sh
```

---

## 🔧 环境配置 (Linux)

```bash
# 1. 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
rustup target add aarch64-unknown-linux-musl

# 2. 安装交叉编译工具链 (musl.cc 预编译工具链，前缀为 aarch64-linux-musl)
wget https://musl.cc/aarch64-linux-musl-cross.tgz
tar -xzf aarch64-linux-musl-cross.tgz
export PATH="$PWD/aarch64-linux-musl-cross/bin:$PATH"

# 3. 安装其余构建依赖
sudo apt install upx-ucl        # UPX 压缩 (build.sh 默认启用)
sudo apt install mtd-utils      # mkfs.ubifs (--pack 打包 UBIFS)

# 4. 验证
rustup target list --installed
which aarch64-linux-musl-gcc
```

> `build.sh` 会自动探测 `aarch64-linux-musl-*` 或 `aarch64-unknown-linux-musl-*` 前缀的工具链，将工具链 `bin` 目录加入 `PATH` 即可。

---

## 📡 ofono D-Bus 接口

### 核心接口

| 接口 | 说明 |
|------|------|
| `org.ofono.Manager` | 调制解调器管理 |
| `org.ofono.Modem` | Modem 属性和控制 |
| `org.ofono.NetworkRegistration` | 网络注册状态 |
| `org.ofono.SimManager` | SIM 卡管理 |
| `org.ofono.ConnectionManager` | 数据连接管理 |
| `org.ofono.VoiceCallManager` | 语音通话管理 |
| `org.ofono.MessageManager` | 短信管理 |

### 常用 D-Bus 命令

```bash
# 查看 Modem 属性
dbus-send --system --print-reply \
  --dest=org.ofono /ril_0 org.ofono.Modem.GetProperties

# 查看网络状态
dbus-send --system --print-reply \
  --dest=org.ofono /ril_0 org.ofono.NetworkRegistration.GetProperties

# 查看 SIM 卡信息
dbus-send --system --print-reply \
  --dest=org.ofono /ril_0 org.ofono.SimManager.GetProperties

# 设置飞行模式
dbus-send --system --print-reply \
  --dest=org.ofono /ril_0 org.ofono.Modem.SetProperty \
  string:"Online" variant:boolean:false

# 发送 AT 指令
dbus-send --system --print-reply \
  --dest=org.ofono /ril_0 org.ofono.Modem.SendAtcmd \
  string:"AT+CGSN"
```

### 监控 D-Bus

```bash
# 监听 ofono 发出的所有信号
dbus-monitor --system "sender='org.ofono'"

# 监听发给 ofono 的调用
dbus-monitor --system "destination='org.ofono'"

# 监听短信信号
dbus-monitor --system "interface='org.ofono.MessageManager'"
```

---

## 📶 频段锁定

仅供参考 真实性有待考证，请以实际设备为准

### LTE (4G) 频段

| 频段 | 位掩码 | 说明 |
|------|--------|------|
| B1 | 1 | FDD 2100MHz |
| B3 | 4 | FDD 1800MHz |
| B5 | 16 | FDD 850MHz |
| B8 | 128 | FDD 900MHz |
| B38 | 32 (TDD) | TDD 2600MHz |
| B40 | 128 (TDD) | TDD 2300MHz |
| B41 | 256 (TDD) | TDD 2500MHz |

### NR (5G) 频段

| 频段 | 位掩码 | 说明 |
|------|--------|------|
| N1 | 1 (FDD) | 2100MHz |
| N28 | 512 (FDD) | 700MHz |
| N41 | 16 (TDD) | 2500MHz |
| N77 | 128 (TDD) | 3700MHz |
| N78 | 256 (TDD) | 3500MHz |
| N79 | 512 (TDD) | 4500MHz |

### AT 指令

```bash
# 查询当前 LTE 频段
AT+SPLBAND=0

# 查询当前 NR 频段
AT+SPLBAND=3

# 锁定 LTE B1+B3
AT+SPLBAND=1,0,0,0,0,5,0

# 锁定 NR N78
AT+SPLBAND=2,0,0,256,0

# 解锁所有频段
AT+SPLBAND=1,0,0,0,0,0,0
AT+SPLBAND=2,0,0,0,0
```

---

## 📈 性能调优（IRQ/RPS，可选）

UDX710 双核上，5G 收包 IRQ（`sprd,sipa`）与 USB 出口 IRQ（`xhci-hcd`）默认落在 CPU0，高频中断单核串行可能限制下行上限。

**系统配置 → 性能调优（IRQ/RPS）** 提供开关（默认关闭），开启后：

1. 将 xhci IRQ 的 `smp_affinity` 固定到 CPU1
2. 为 `sipa_eth0` / `usb0` 打开 RPS（`rps_cpus=3`）
3. 扩大 `rps_sock_flow_entries` 至 32768

设置持久化在 `config.json`，启动时自动应用；网卡重建后由既有健康巡检幂等补齐。关闭开关即恢复默认。

> 是否提速取决于信号与链路条件（弱信号场景可能无明显变化），请自行 A/B 对比验证。本项目不承诺具体吞吐数字。

也可手动一次性应用（重启失效）：

```sh
# xhci USB IRQ → CPU1（IRQ 号请先 cat /proc/interrupts 确认）
echo 2 > /proc/irq/97/smp_affinity

for d in sipa_eth0 usb0; do
  for q in /sys/class/net/$d/queues/rx-*/rps_cpus; do
    echo 3 > "$q"
  done
done

echo 32768 > /proc/sys/net/core/rps_sock_flow_entries
```

---

## 📚 API 接口文档

### 基础信息
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/health` | GET | 健康检查 |
| `/api/device` | GET | 设备信息 (IMEI/ICCID/型号) |
| `/api/device/imeisv` | GET | 软件版本号 |
| `/api/sim` | GET | SIM 卡信息 |
| `/api/sim/slot` | GET | SIM 卡槽状态 |
| `/api/sim/slot/switch` | POST | 切换 SIM 卡槽 |

### 网络状态
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/network` | GET | 网络注册信息 |
| `/api/network/interfaces` | GET | 网络接口信息 |
| `/api/network/signal-strength` | GET | 信号强度 |
| `/api/network/nitz` | GET | 网络时间 |
| `/api/network/operators` | GET | 运营商列表 |
| `/api/network/operators/scan` | GET | 扫描运营商 (耗时) |
| `/api/network/register-manual` | POST | 手动注册运营商 |
| `/api/network/register-auto` | POST | 自动注册运营商 |
| `/api/cells` | GET | 基站信息 |
| `/api/location/cell-info` | GET | 基站定位参数 |
| `/api/qos` | GET | QoS 信息 |

### 模块控制
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/data` | GET/POST | 数据连接开关 |
| `/api/roaming` | GET/POST | 漫游开关 |
| `/api/airplane-mode` | GET/POST | 飞行模式开关 |
| `/api/radio-mode` | GET/POST | 射频模式 (4G/5G/自动) |
| `/api/band-lock` | GET/POST | 频段锁定 |
| `/api/cell-lock` | GET/POST | 小区锁定 |
| `/api/cell-lock/unlock-all` | POST | 解锁所有小区 |
| `/api/apn` | GET/POST | APN 配置 |
| `/api/usb-mode` | GET/POST | USB 模式切换 |
| `/api/usb-advance` | POST | 高级 USB 模式设置 |

### 通话功能
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/calls` | GET | 当前通话列表 |
| `/api/call/dial` | POST | 拨打电话 |
| `/api/call/hangup` | POST | 挂断指定电话 |
| `/api/call/hangup-all` | POST | 挂断所有电话 |
| `/api/call/answer` | POST | 接听来电 |
| `/api/call/volume` | GET/POST | 通话音量设置 |
| `/api/call/forwarding` | GET/POST | 呼叫转移设置 |
| `/api/call/settings` | GET/POST | 通话设置 |
| `/api/call/history` | GET | 通话记录列表 |
| `/api/call/history/{id}` | DELETE | 删除指定通话记录 |
| `/api/call/history/clear` | POST | 清空通话记录 |

### 短信功能
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/sms/send` | POST | 发送短信 |
| `/api/sms/list` | GET | 短信列表 |
| `/api/sms/conversation` | GET | 短信会话列表 |
| `/api/sms/stats` | GET | 短信统计 |
| `/api/sms/clear` | POST | 清空短信 |

### IMS/VoLTE
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/ims/status` | GET | IMS 状态 |
| `/api/voicemail/status` | GET | 语音信箱状态 |

### 系统信息
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/stats` | GET | 系统统计（网速/内存/运行时间） |
| `/api/stats/cpu` | GET | CPU 信息 |
| `/api/connectivity` | GET | 网络连通性检查 |
| `/api/system/reboot` | POST | 重启系统 |
| `/api/at` | POST | 执行 AT 指令 |

### Webhook 配置
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/webhook/config` | GET/POST | Webhook 配置管理 |
| `/api/webhook/test` | POST | 测试 Webhook |

### OTA 更新
| 接口 | 方法 | 说明 |
|------|------|------|
| `/api/ota/status` | GET | OTA 更新状态 |
| `/api/perf-tune` | GET/POST | IRQ/RPS 性能调优开关 |
| `/api/ota/upload` | POST | 上传 OTA 包 (最大 50MB) |
| `/api/ota/apply` | POST | 应用 OTA 更新 |
| `/api/ota/cancel` | POST | 取消 OTA 更新 |

---

## 🛠 开发指南

### D-Bus 操作序列化

所有 D-Bus/AT 操作必须通过 `with_serial` 串行执行：

```rust
use crate::serial::with_serial;

pub async fn send_at_command(conn: &Connection, cmd: &str) -> zbus::Result<String> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        proxy.call("SendAtcmd", &(cmd)).await
    }).await
}
```

### API 响应格式

```rust
#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub status: String,   // "ok" 或 "error"
    pub message: String,
    pub data: Option<T>,
}
```

---

## 📦 依赖

- **zbus 5.x** - D-Bus 客户端
- **tokio 1.48** - 异步运行时
- **axum 0.8** - Web 框架
- **rusqlite 0.32** - SQLite (bundled)
- **tower-http 0.6** - HTTP 中间件

---

## license 许可证

GNU General Public License v3.0
