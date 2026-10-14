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
| HDR / EDID | 原生视频菜单和设置页调用共享板级助手，与 vim4-manager 共用 HDR 曲线 |

应用以 khadas 运行，不创建、删除 USB gadget，不重启 HDMI 服务。
仅用户明确应用 EDID 时调用共享助手重新协商显示器；普通观看、调码率与缩放不触发 HPD。
虚拟介质经限定 sudo 助手操作 configfs，镜像只允许来自
`/var/lib/one-kvm/msd/images` 或 `ventoy`，拒绝符号链接与硬链接。
不支持多 LUN、应用修改 USB 描述符、USB 麦克风或 OTG 网桥；ATX 未连接。
Ventoy 创建仍需要上游的 Ventoy 资源，普通 ISO/IMG 不依赖这些资源。

## 视频设置

输入状态从 HDMI RX sysfs 读取，不打开独占的 VFM 采集节点；显示实际输入分辨率
与小数帧率。YUV420 输入的时钟域半宽转换为完整画面宽度。NV21 是采集输出格式，
不代表 HDMI 线上的颜色编码或位深。

`PATCH /api/config/video` 的 `output: {height, fps}` 独立于跟随源信号的采集参数。
height 为 0（原尺寸）、720、1080、2160；fps 为 0（跟随输入）、10、15、30、60。
尺寸和帧率均为上限，保持宽高比、不放大；缩放通过 gstreamer-aml 的 GE2D
`amlvconv` 完成。MJPEG 软件编码最高 10 fps，`quality` 1–100 在重启与调整时应用。
H.264/H.265 支持三个预设和 1000–12000 kbps 自定义码率，越界请求返回错误。

`GET/POST /api/vim4/video` 需要原有会话认证。EDID 提供 4K60 和 1080p60 两个预设，
使用 NanoKVM 包的共享 vim4-edid 助手；其他应用主动恢复自己的 EDID 时可以覆盖
当前显示能力。HDR 白点 160–1000 nits，峰值大于白点且不超过 10000 nits，使用
vim4-manager 的 hdr-curve 助手保存、立即重载；SDR 输入不受影响，HLG 仍用厂商曲线。

## 构建与安装

在工作区固定的 ARM64 Ubuntu 构建环境运行：

```sh
npm --prefix web ci --ignore-scripts
npm --prefix web run build
sh deploy/vim4/build-dma-allocator.sh
cargo test --locked --features vim4 --lib
cargo build --locked --release --features vim4
python3 -m unittest discover -s deploy/vim4 -p 'test_*.py'
```

Rust 版本由 rust-toolchain.toml 固定；系统依赖在公共工作区的
packaging/ubuntu/Dockerfile 固定。Ubuntu libyuv 使用兼容的两步 BGR 转换；
静态 turbojpeg 符号隐藏，防止与系统 libyuv 的 JPEG ABI 冲突。
GE2D 缩放使用私有 `libgst-aml-dma-allocator.so`，仅实现 amlvconv 调用的
分配器入口，将 DMA fd 的关闭权交给标准 GstDmaBufMemory。厂商版本同时在
附属 qdata 中关闭 fd，反复缩放/切编码会误关复用的 fd。服务的 LD_LIBRARY_PATH
只为 One-KVM 加载此兼容库；不替换系统库或影响其他 KVM。构建包含 fd 生命周期测试。

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
