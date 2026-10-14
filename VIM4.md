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
| 多媒体键 | 独立 `/dev/hidg3`，16-bit Consumer usage |
| ISO/IMG | 板级 `mass_storage.disk0/lun.0`，单 LUN；共享 USB 端点预算 |
| USB 麦克风 | 板级 UAC2，48 kHz 双声道；浏览器经认证 WebSocket 发送 Opus/PCM |
| USB 网卡 | NCM / ECM / RNDIS，独立 NetworkManager DHCP/NAT 连接 |
| HDR / EDID | 原生视频菜单和设置页调用共享板级助手，与 vim4-manager 共用 HDR 曲线 |

应用以 khadas 运行，不创建、删除 USB gadget，不重启 HDMI 服务。
仅用户明确应用 EDID 时调用共享助手重新协商显示器；普通观看、调码率与缩放不触发 HPD。
虚拟介质经限定 sudo 助手操作 configfs，镜像只允许来自
`/var/lib/one-kvm/msd/images` 或 `ventoy`，拒绝符号链接与硬链接。
不支持多 LUN、以太网桥接；ATX 未连接。
Ubuntu 包将仓库内的 Ventoy 引导资源解压到 `/usr/share/vim4-onekvm/ventoy`，
用于创建虚拟盘；用户盘保存在 `/var/lib/one-kvm/msd/ventoy`，升级不覆盖。
普通 ISO/IMG 不依赖这些引导资源。虚拟盘测试使用同一套资源，缺失或解压失败时必须失败。

## HID 功能配置

设置页支持键盘、绝对/相对鼠标、多媒体键、键盘 LED 状态反馈、USB VID/PID、
厂商/产品名/序列号、虚拟盘及磁盘名称、USB 网卡和麦克风。`PATCH /api/config/otg`
可同时提交 `hid`、`msd`、`network`、`uac`，应用失败后回滚，避免麦克风单独保存
导致中间组合超过端点上限。绝对鼠标单独启用时，按钮和滚轮使用绝对接口。

VIM4 CRG 限制同时使用最多 5 个 IN 端点：每个 HID、麦克风、已挂载磁盘各一个，
网卡两个。设置页和 API 在启用虚拟盘时预留一个；助手在实际挂载时再次校验。
只读的存储目录是 `/var/lib/one-kvm/msd`，不能用网页配置绕过助手的路径限制。
USB 网卡不改变已有以太网配置；地址、DHCP 与 NAT 由 NetworkManager shared 连接
管理。RNDIS 的 Windows 驱动兼容性仍需在 Windows 主机验收。

应用在重新枚举前关闭 HID 文件，助手仅修改公共 gadget 的功能链接与描述符，
不销毁现有 HID 实例；设备编号保持稳定。首次应用前在 root 专用的
`/run/vim4-onekvm-usb` 保存原始状态，systemd `ExecStopPost` 恢复配置并删除本应用
创建的网络连接，因此切换到其他 KVM 不继承 One-KVM 的键鼠禁用设置。
保存会重新枚举 USB，用户应先在被控机安全弹出挂载磁盘。网页关闭不等于停止服务。

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
应使用工作区构建的 Ubuntu deb。网页升级仅接收 `vim4-ubuntu-arm64` 制品和
`x.y.z+vim4.YYYYMMDD.N` 版本，校验大小与 SHA256，再由限定助手验证包名、架构、
版本及 APT 依赖计划。需要改动平台依赖时拒绝单应用升级；安装核验后重启应用。
默认更新源是 vim4-kvm/vim4-kvm 的 GitHub Release；构建包附带
`onekvm-channels.json`、`onekvm-releases.json`，发布时与 deb 一起上传。
没有发布清单时界面明确报告暂无此通道更新，不回退到上游通用二进制。

## 终端、扩展与看门狗

网页终端使用 Ubuntu 的 ttyd，以 khadas 运行，经现有认证代理访问专用 Unix socket。
包内私有副本只带程序和许可证，不安装或启用 Ubuntu 的独立 ttyd 守护服务。
systemd 创建 `/run/one-kvm`（0700），不另外监听公网终端端口。
Gostc 2.0.9、EasyTier 2.4.5、FRP 0.71.0 使用 SHA256 固定的 ARM64 上游制品，
安装在 `/opt/vim4-onekvm/bin`，保留许可证；不覆盖其他应用的命令。
扩展默认关闭，远程连通需要用户填写自己的服务器配置。只有 EasyTier 程序带
`cap_net_admin`，用于其 TUN 网络；应用本身不获得该能力。扩展启停日志不记录含密钥的参数。

看门狗设备通过独立 `vim4-watchdog` 组开放给服务，未把 khadas 加入全局设备组。
网页开启后应用负责喂狗，正常关闭或切换应用时调用原有安全关闭流程。
不修改 nowayout；禁止用“强杀应用导致复位”代替普通部署验收。

## 切换

vim4-manager 在 80 端口选择 One-KVM、NanoKVM 或 JetKVM，应用共用 8080。
助手等待旧应用完全退出再启动所选应用，并保存开机选择。三者共用硬件，
不能并发启动。USB 虚拟介质挂载会重新枚举 USB，测试前应协调独占板子。

软件测试不代表硬件验证；当前板子上的实测证据记录在公共工作区
`docs/bringup-log.md`。
