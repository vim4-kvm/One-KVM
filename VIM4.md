# VIM4 Ubuntu

使用 `vim4` Cargo feature 构建 Khadas VIM4 Ubuntu 24.04 版本，保留上游网页、
认证、WebRTC、音频和虚拟介质业务。公共板级服务由
[vim4-kvm 工作区](https://github.com/vim4-kvm/vim4-kvm) 提供。

## 硬件映射

| 功能 | 实现 |
|---|---|
| HDMI 输入 | vdin0 → `/dev/video_cap` → `vfmcapsrc`，跟随输入分辨率 |
| WebRTC 视频 | DMA-buf → `amlvenc` → H.264/H.265 Annex B；使用现有会话及关键帧恢复 |
| MJPEG | vim4jpegenc DMA 硬件 JPEG，遵循输出帧率上限；缺少插件时软件回退最高 10 fps |
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
`amlvconv` 完成。MJPEG 使用 `vim4jpegenc` 硬件编码，`quality` 1–100 在重启与调整时应用；
缺少插件时保留最高 10 fps 的软件回退。实际帧率受 JPEG 编码、缩放、网络和浏览器
限制，状态显示实际编码输出帧率，不将设定的上限当作实测值。
H.264/H.265 支持 1/4/8/12 Mbps 四个快捷档和 1000–60000 kbps 自定义码率，
越界请求返回错误。部署时须同时升级支持 60000 kbps 的 vim4-amlvenc 插件；
旧插件上限为 12000 kbps。高码率需要更多网络带宽，实际输出随画面变化。

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
从仓库根目录运行 Cargo，使 `.cargo/config.toml` 的 ARMv8 AES/PMULL 配置生效；
依赖运行时检测 CPU 能力，不支持时回退软件实现。不要用外部 `RUSTFLAGS` 覆盖
这些配置，否则大关键帧的 SRTP 加密可能阻塞发送，出现每秒一次的停顿。
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

## UU 远程

设置页“第三方访问”的 UU 远程卡片让网易UU远程官方 Windows/Mac/手机客户端经网易云端
控制被控机，非网易官方功能。协议行为来自 uu-remote-linux（GPL-3.0-or-later，见
`src/uu_remote/NOTICE`），网页可选择 Windows 或 Mac 身份，默认 Windows，设备名称默认
`vim4-kvm`，支持修改（去除首尾空白后 1–64 字符，无控制字符）。系统和名称保存到应用配置。
UU 云端按平台分配不同设备 ID，首次切换到另一系统需要重新扫码，切换会断开当前观看。
Windows 与 Mac 的登录分别存储，切回恢复对应身份。网页生成二维码，用 UU 手机 App
扫码确认后绑定账号；二维码等同登录凭据，只返回给已登录的网页会话，日志不记录二维码、
令牌或 TURN 凭据。账号与设备标识保存在 `/var/lib/one-kvm/uu-remote/credentials.json`
（Mac 使用同目录 `credentials-macos.json`；目录 0700、文件 0600），不进入应用配置和配置 API；
解除绑定仅清除当前平台的登录并保留设备标识，另一平台的登录不受影响。
令牌失效（API 1120）时服务回到等待登录状态。
连接后 30 秒起刷新信令重连密钥，成功后每 300 秒、失败后每 30 秒一次；不刷新时云端约 20 分钟回收房间。
房间续期或信令重连只替换信令连接，已建立的桌面连接保持；旧房间的读写任务随之停止。

| 功能 | 实现 |
|---|---|
| 画面 | 共享 H.264/H.265 编码管线，附 UU 采集源 RTP 扩展；从关键帧开始，PLI/FIR 请求关键帧 |
| 声音 | 应用已启用的音频采集，Opus 直接转发 |
| 键鼠 | 按所选设备身份解析 Windows VK 或 macOS 虚拟键码，再按美式物理位置映射到 HID；支持常用键、左右修饰键、方向键和功能键；归一化绝对坐标、相对移动、滚轮。切到 Mac 默认反转滚轮，Windows 默认不反转，网页可手动覆盖并保存；方向变化会断开 UU 会话，重连后生效。Mac 的 Fn、媒体键及部分 JIS 专用键尚不支持 |
| 画质 | 快速/高清/超清/原画/自定义映射到共享码率（1/2/8/20 Mbps、1000–60000 kbps），同步自定义码率 |
| 输出 | UU 合并设置与单独帧率、分辨率命令均生效；帧率上限 5–60，支持原尺寸与等比例缩小，不放大 |
| 菜单命令 | 显示桌面、锁定、任务管理器以 Win+D、Win+L、Ctrl+Shift+Esc 发送 |

网页 UU 设置支持自动、H.264、H.265。自动模式优先协商 H.265 Main 8-bit，
客户端未提供兼容 H.265 或其他服务锁定 H.264 时回退 H.264；固定编码不兼容则拒绝连接。
会话期间锁定共享编码器为实际协商的编码，关闭会话后释放；不能与 MJPEG 类第三方访问并存。
更改编码设置会断开 UU 会话，重连后生效；网页已有观看会话可能需要按新编码重连。
观看端修改画质、帧率或尺寸会重建共享编码管线，同时影响网页画面；保留媒体订阅与连接。
只有新设置产出编码帧后才回复成功并同步实际尺寸与帧率上限；失败记录日志，不回复虚假成功。
自定义尺寸保持输入宽高比，输出高度取偶数（180–2160）；实际帧率不超过输入帧率。
UU 客户端设置仅在当前运行期生效，不保存到网页配置；网页 UU 卡片旁提供相同的视频控制，
网页保存的输出上限和码率会持久化。刷新控件读取当前运行设置，网页改码率也同步到 UU 客户端。
自动画质恢复连接开始时的码率，并非网络自适应码率。
设定帧率是上限，不是吞吐保证：4K60 输入上板短测直出约 60fps，GE2D 缩放至 1080p/720p
分别约 35/40fps；低帧率档已验证，缩放 60fps 尚未实现。
一次只服务一个桌面连接，新连接替换旧连接。
连接断开时释放本连接按住的键和鼠标按钮。文件传输、终端、端口映射会暴露 KVM 板本身，
不提供；剪贴板、静音、远程结束自动锁屏、自动解锁和指针形状未声明能力。
被控机锁屏状态无法观测，始终上报未锁定。ICE 不使用 usb*、容器与覆盖网络接口。

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
