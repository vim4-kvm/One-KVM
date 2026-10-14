# VIM4 Ubuntu

使用 `vim4` Cargo feature 构建 Khadas VIM4 Ubuntu 24.04 版本，保留上游网页、
认证、WebRTC、音频和虚拟介质业务。公共板级服务由
[vim4-kvm 工作区](https://github.com/vim4-kvm/vim4-kvm) 提供。

## 硬件映射

| 功能 | 实现 |
|---|---|
| HDMI 输入 | vdin0 → `/dev/video_cap` → `vfmcapsrc`，跟随输入分辨率 |
| WebRTC 视频 | DMA-buf → `amlvenc` → H.264/H.265 Annex B；使用现有会话及关键帧恢复 |
| MJPEG | GStreamer 软件 JPEG，限制为 10 fps；4K60 使用 WebRTC |
| 声音 | ALSA `hw:AMLAUGESOUND,0` → 上游 Opus/WebRTC；板级服务设置 HDMI 音频路由 |
| 键盘 / 绝对鼠标 / 相对鼠标 | `/dev/hidg0` / `/dev/hidg1` / `/dev/hidg2` |
| ISO/IMG | 板级 `mass_storage.disk0/lun.0`，单 LUN；挂载时临时替换 USB NCM |
| HDR / EDID | 共享板级 HDMI 配置，HDR 曲线在 vim4-manager 调节 |

应用以 khadas 运行，不创建、删除 USB gadget，不改 HDMI EDID，不重启 HDMI 服务。
虚拟介质经限定 sudo 助手操作 configfs，镜像只允许来自
`/var/lib/one-kvm/msd/images` 或 `ventoy`，拒绝符号链接与硬链接。
不支持多 LUN、应用修改 USB 描述符、USB 麦克风或 OTG 网桥；ATX 未连接。
Ventoy 创建仍需要上游的 Ventoy 资源，普通 ISO/IMG 不依赖这些资源。

## 构建与安装

在工作区固定的 ARM64 Ubuntu 构建环境运行：

```sh
npm --prefix web ci --ignore-scripts
npm --prefix web run build
cargo test --locked --features vim4 --lib
cargo build --locked --release --features vim4
python3 -m unittest discover -s deploy/vim4 -p 'test_*.py'
```

Rust 版本由 rust-toolchain.toml 固定；系统依赖在公共工作区的
packaging/ubuntu/Dockerfile 固定。Ubuntu libyuv 使用兼容的两步 BGR 转换；
静态 turbojpeg 符号隐藏，防止与系统 libyuv 的 JPEG ABI 冲突。

工作区 `build onekvm` 输出 `build/onekvm`；`vim4-onekvm` deb 安装到
`/opt/vim4-onekvm`，数据目录为 `/var/lib/one-kvm`。安装不创建账户，首次网页
设置独立账户；升级保留配置与账户。禁止安装上游通用 ARM/Rockchip 在线更新包，
应使用工作区构建的 Ubuntu deb。

## 切换

vim4-manager 在 80 端口选择 One-KVM、NanoKVM 或 JetKVM，应用共用 8080。
助手等待旧应用完全退出再启动所选应用，并保存开机选择。三者共用硬件，
不能并发启动。USB 虚拟介质挂载会重新枚举 USB，测试前应协调独占板子。

软件测试不代表硬件验证；当前板子上的实测证据记录在公共工作区
`docs/bringup-log.md`。
