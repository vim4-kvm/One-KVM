# VIM4 Ubuntu

本页管集成与验收；参数见源码。
先更新配套平台包再更新应用；离线通过不代表上板通过。
按任务读取：[安装](#构建与安装)、[USB 重枚举](#hid-功能配置)、[介质](#虚拟介质状态)。
视频任务：[输出设置](#视频设置)、[延迟诊断](#视频延迟诊断)、[HDR](#hdr-与画质)。
业务任务：[广播](#共享编码-srt--rtmp-输出)、[录屏](#设备端录屏)、[UU](#uu-远程)。
访问任务：[鉴权](#网页鉴权)、[MCP](#mcp)、[控制台](#控制台与终端)；[切换边界](#切换)。

## 构建与安装

保留上游网页、WebRTC、音频和虚拟介质业务；VIM4 差异由
[Cargo feature](Cargo.toml)、[平台适配](src/platform/vim4.rs)和
[服务](deploy/vim4/one-kvm.service)隔离。公共启动器按内核选择厂商或 V4L2/Wave5 后端，
账户、镜像和设置跨内核保留。独立实验服务的隔离边界见
[Wave5 说明](deploy/vim4/WAVE5.md)，不要将实验目录当作正式服务目录。

在公共工作区构建、核验组件后部署，入口及参数见
[构建工具](https://github.com/vim4-kvm/vim4-kvm/blob/main/tools/ubuntu-build.py)、
[构建依赖](https://github.com/vim4-kvm/vim4-kvm/blob/main/packaging/ubuntu/Dockerfile)。
先更新匹配的启动器、驱动、采集与编码包，再更新应用；依赖契约以
[打包实现](https://github.com/vim4-kvm/vim4-kvm/blob/main/packaging/ubuntu/package.py)为准。
私有标准 V4L2 插件须优先于系统厂商替代插件，后者会关闭主 HDMI RX。
独立构建该插件可在主仓根目录运行：
`python3 packaging/kernel/build-gstreamer.py build/mainline-gstreamer`，
输出目录须为空；应用组件打包会自动完成此步骤。

子仓库构建与离线回归：
```sh
npm --prefix web ci --ignore-scripts
npm --prefix web run build
sh deploy/vim4/build-dma-allocator.sh
cargo test --locked --features vim4 --lib
cargo build --locked --release --features vim4
python3 -m unittest discover -s deploy/vim4 -p 'test_*.py'
```
从仓库根运行 Cargo，勿覆盖 [.cargo/config.toml](.cargo/config.toml) 的 CPU 配置。
TurboJPEG 须隐藏符号，避免 libyuv 的 JPEG ABI 冲突。
SRTP 的依赖条件与测试见 [SRTP 后端](libs/srtp/VIM4.md)；
硬件加速不证明尾延迟已解决。

助手测试在独立 worktree 须指定公共 board 目录，见[测试](deploy/vim4/test_helper.py)。
[SSH 回环](src/uu_remote/terminal/tests.rs)默认跳过，只在隔离容器准备并运行。
前端入口见 [package.json](web/package.json)；临时覆盖结束后移除 drop-in、重启恢复内嵌网页。

安装不创建 Ubuntu 账户；升级保留用户数据。只能安装本工作区的 Ubuntu 制品，
禁止上游通用 ARM/Rockchip 在线更新包。更新校验、通道、开发版本排序及包管理锁处理见
[更新实现](src/update/vim4.rs)和[限定助手](deploy/vim4/vim4-onekvm-update)。
平台依赖变化会拒绝单应用升级；无清单不回退上游。开发包用工作区部署。
Ventoy 引导资源随包提供，用户盘升级不覆盖；普通 ISO/IMG 不依赖它。
资源缺失或解压失败须报错，虚拟盘测试不能用空资源。

## HID 功能配置

先在被控机安全弹出介质，再更改 USB 组合；网页关闭不等于停止服务。
挂载介质期间拒绝配置变更，停止应用仍保留主机在用介质，安全弹出后才恢复基线。
勿混用正式与实验助手。

端点预算、功能映射和合并规则只读
[助手：profile_links、merged_consumer、validate_profile](deploy/vim4/vim4-onekvm)；
设置字段与设备能力只读 [平台实现](src/platform/vim4.rs)、
[串口能力探测](src/platform/vim4/usb_serial.rs)。
网卡和 ACM 比单个 HID 消耗更多 IN 端点；磁盘在挂载时才占预算，
所以配置可保存不等于之后一定能挂盘。端点不足先调整组合，不绕过助手。
多媒体键与绝对鼠标可合并；旧程序未声明合并能力时保留独立接口，
避免向带 Report ID 的接口写入旧格式报告。

依次关闭 HID、保存链接与描述符、助手重枚举、重开 HID。
助手不销毁已有 HID 实例，保持设备编号稳定；原始报告长度和描述符随基线恢复。
应用组合一起提交，失败回滚，避免分步保存产生超预算的中间组合。
服务退出由 [ExecStopPost](deploy/vim4/one-kvm.service) 调用
[restore_profile](deploy/vim4/vim4-onekvm)，恢复原组合并清理应用创建的网络连接，
防止下个 KVM 继承禁用的键鼠。应用不管理公共 gadget 的创建与删除。

ACM 只读探测现有函数、已加载模块或当前内核模块，不能用创建函数、加载模块来探测。
必须安装与内核匹配的完整模块；模块文件存在仍不保证能加载。
创建失败须在解绑前报出具体错误。串口主机枚举与兼容边界见
[UU 终端](#uu-终端)，RNDIS 仍需 Windows 主机验收。
USB 网卡由 NetworkManager 的共享连接管理，不改变原以太网配置，不支持桥接。

滚轮已含本机自然滚动，“反转”按被控机手选，不按浏览器系统判断。指针设置只存浏览器；
相对锁定时的本地箭头不代表被控机光标的真实形状。

## 虚拟介质状态

列表按来源分组；其他 KVM 镜像不可删除、重命名或写入，ISO 强制光盘；
共享数据盘可选只读或可写。目录边界、共享锁与编辑锁由
[限定助手：open_image、configure_media](deploy/vim4/vim4-onekvm)和公共
[介质实现](https://github.com/vim4-kvm/vim4-kvm/blob/main/board/vim4media.py)共同约束，
拒绝路径穿越、符号链接和硬链接。

只有本进程成功挂载且仍在原 LUN 上的同一介质才属于本应用；启动前就挂载的
本地镜像和 Ventoy 盘也算外部介质。外部介质禁止应用内挂载、弹出，
须先在被控机安全弹出，再从管理面板弹出。助手在共享锁内再次核对预期介质，
拒绝时不清空 LUN、不解绑 USB。外部弹出后清除旧记录；本页面挂载的目录外镜像可正常弹出。
状态由实际 LUN 回读，不能把旧进程记录当作所有权凭据。

假 LUN 与助手离线测试不模拟 configfs、SCSI 防弹出锁或 USB 枚举，仍须独立上板验收。
单 LUN 与共享端点是集成边界，切换应用不自动迁移挂载状态。

## 视频设置

输入状态读取 RX 状态，不打开独占采集节点；采集格式不代表 HDMI 线上的编码或位深。
输入解释与失锁见 [input_status、parse_capture_signal](src/platform/vim4.rs)，
主线探测见 [video_backend](src/platform/vim4/video_backend.rs)。
缓存时序可能在失锁后残留；不能仅凭旧尺寸认定信号可用。
使用接收器的稳定采集基准处理 VRR，避免瞬时帧周期触发重建；
旧驱动保留实测率回退，明确未知的基准不当作可用模式。
这些兼容行为不代表 EDID 宣告的全部 VRR 模式已验收。

输出为上限，等比例缩小；范围与高刷条件见
[OutputSettings](src/platform/vim4.rs)及
[管线：output_fps、description、vendor_capture](src/video/pipeline/shared/vim4.rs)。
源变化先稳定确认再重建，保留观看连接；静止像素不算采集卡死，
真正无帧才触发恢复。软件重开不能消除 HDMI 重新锁定的等待。

保留真实 VideoMeta 行跨度。超宽屏补齐行距时不能把可见宽度当 pitch；
SDR 缩放走 GE2D，HDR 走 Mali/P010，不能退回昂贵的全帧 CPU 路径。
编码器与采集包必须配套升级，否则可能协商成功但宽高比错误或首帧停住。
MJPEG 优先硬件 JPEG，缺插件时软件回退；设定上限不是实际编码帧率，
吞吐还受缩放、内容、网络和浏览器限制。

Wave5 可见尺寸和质量能力按实际 V4L2 探测，失败时回退 H.264 并重新协商；
旧运行时回到对齐模式后不会自动恢复 HEVC，可重新选择。
能力细节与未验项见 [Wave5 能力](deploy/vim4/WAVE5-CAPABILITIES.md)。
EDID 应用先释放采集再恢复会话；普通观看、调码率和缩放不触发 HPD。
共享 EDID 被其他应用主动恢复时可能覆盖当前选择；声明模式不等于真实输入通过。
高刷及 VRR 历史验收见
[记录](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/2026-10-26-vrr-live.md)，
HDR VRR、主线和本地输出的未验边界不可由该记录外推。

## 视频延迟诊断

先用运动画面复现，保持页面前台；同时区分采集、缩放、编码、发送、浏览器缓冲和呈现。
[管线](src/video/pipeline/shared/vim4.rs)给出阶段统计开关，
[videoDiagnostics](web/src/lib/videoDiagnostics.ts)给出窗口和有效性条件；
窗口完整且前台才可比。回调漏采不等于丢帧，呈现提交不是屏幕发光。
连接统计由 [useWebRTC](web/src/composables/useWebRTC.ts)统一采样；
接收至显示仅覆盖浏览器端，不能冒充端到端延迟。
测量端点和当前证据见
[延迟记录](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/2026-10-27-current-video-latency.md)。

排查关键帧突发先读
[厂商管线：description](src/video/pipeline/shared/vim4.rs)、
[Wave5 管线](src/video/pipeline/shared/vim4/wave5.rs)和
[视频发送](src/webrtc/video_track.rs)，不要从旧文档复制 GOP/QP 参数。
周期刷新减少整幅 I 帧突发，但新观看者和丢包仍需完整关键帧；
行刷新不能证明参考链已恢复，必须测试新订阅、重连与 PLI/FIR。
编码预算要跟随输入与丢帧上限，预算不是采集节奏。
RTP 单调时间在丢帧后保留间隔；建连时清理等待期积压，从新关键帧开始。
音视频使用独立 stream ID，减少视频等待音频；拥塞下不保证严格口型同步。
网页协商的 playout-delay 是接收端提示；须比较实际、目标和最低 jitter buffer
驻留时间，不能只看网络 jitter 或平均帧率。它不保证零延迟，也不适用于 UU。

缩放排查见 [DMA 分配器](deploy/vim4/dma-allocator.c)：共享视图全部释放后才复用 fd，
不能由厂商 qdata 再次关闭而误关复用描述符；有界非丢帧队列让缩放与编码重叠。
私有兼容库只在本服务加载。连续采集布局避免厂商转换器漏放多 fd 平面导致整池 CMA 残留；
不能以增加 CMA 代替生命周期修复。先预留编码器内存，再分配采集/GPU 池，
首帧后转交 SDK。池预算在管线与采集插件源码维护。
缩放时钟助手只读核验，警告后仍出流；旧内核共享总线降频会降低吞吐，
须升级驱动，不能将目标帧率报告为实测值。

UDP 批处理实验的范围、取消与背压见
[实现说明](libs/webrtc-util/VIM4.md)，比较完整发送路径及浏览器尾延迟后才能决定启用。
CPU 回放见 [RTP 基准](test/rtp-bench/README.md)，不能代替实时链路。
UU 的发送和接收统计边界见 [UU 远程](#uu-远程)。

## HDR 与画质

全局远程 HDR 转 SDR 偏好由管理器维护；输出尺寸、帧率保存不能覆盖它。
全局值缺失时保留应用兼容偏好；API 省略 HDR 字段不写默认。
本地 HDMI OUT 偏好独立。权威选择逻辑见
[全局偏好](src/platform/vim4.rs)、
[管线：native_hdr、hdr_description](src/video/pipeline/shared/vim4.rs)。

厂商原生双路径让 VDIN0 保留显示/HDR，VDIN1 提供 SDR；原厂写回设备不能直接替代配套驱动。
PQ 原生 HEVC 需匹配的采集、私有 SDK 与浏览器 Main10 能力；SDR 不因 HDR 偏好增加 P010 转换。
HDR 缩放保留十位精度和 PQ 色彩，用 Mali 转换后 DMA 输入编码器。
八位 HEVC 的宽度对齐与 Main10 的行宽/裁剪修复须随 SDK 部署，
否则客户端的任意缩小尺寸可能首帧失败并在重试后继续停帧。
EOTF 变化重建管线但保留用户偏好；色彩须看实际参数集与 RTP 信息，不能只看编码名称。
HLG 未验收，客户端连续显示与实际屏幕光度须分别验证，发送帧率不能证明 HDR 显示。
历史 GPU/缩放测量见
[HDR 缩放](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/2026-10-27-uu-hdr-scale.md)。

UU 桌面先用 SDR，由客户端明确开启 HDR；解码能力不等于显示能力。
仅匹配当前尺寸的十位能力生效，不改写标准浏览器 SDP；不支持 HDR 显示时拒绝开启。
HDR 临时设置不落库，断开或接管后恢复全局值，保留输出参数。
工具连接不改 HDR。合并请求的不可用 HDR 偏好不阻塞尺寸和码率调整，
单独开启仍须能力检查。原生 UU 能力解释以
[capabilities](src/uu_remote/capabilities.rs)为准，避免按某版客户端 SDP 特例硬编码操作。

固定 QP/无损共享于网页、UU、广播和录屏；H.264 不受 HEVC 偏好影响。
无损只保证编码输入 YUV，不补偿颜色转换、色度抽样或缩放，不保证复杂内容实时编码。
不同后端暴露的模式按实际能力，回到支持后端可恢复保存偏好。
回退旧应用前先切回目标码率，避免旧配置解析器拒绝新增模式；
编码器依赖契约须先满足。

主线原生 HDR 截图候选从共享 P010 池借帧，经 GPU 色调映射后走 SDR JPEG；
空闲阀门不持帧，取消、停流释放借用，缺配套 GPU 元素明确失败。
不再开 VDIN，不切共享编码器，不能用截图的 SDR caps 覆盖主路径 PQ。
离线通过不代表两种 GPU 栈、截图或实际 UU 客户端已验收，见
[主线候选记录索引](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/README.md)。

## 共享编码 SRT / RTMP 输出

配置与范围见 [broadcast](src/broadcast/mod.rs)；输出由设备进程持有，关网页仍发送，
共享编码与音频，不额外开编码器，拥塞不阻塞 KVM。
跟随当前格式，RTMP 等待兼容编码而不主动切换，SRT 也不接受 MJPEG。
音频开启须有真实采集，不静默回退无音频；只将共享 Opus 转 AAC。
新接收端、拥塞、断线或格式变化后从新关键帧重建，一路修改不重启另一路。

接收地址从页面复制，选项与限制读配置实现，不把重传窗口当端到端延迟。
配置 API 不回传凭据，空输入保留原值，清除须显式操作。
时间轴复用媒体时间，复用管线不另声明实时源，避免 FLV 把稍迟音频挤到视频时间上。
网络插件发送字节不证明接收端已解码。

## 设备端录屏

[录制生命周期](src/recording/mod.rs)、[封装](src/recording/mux.rs)、
[存储](src/recording/storage.rs)和[位置](src/recording/locations.rs)
维护接口、限额与路径，网页行为见 [录制 API](src/web/handlers/recording.rs)。
设备持有录制；关网页、刷新、网页静音不停止或改变录音。
开始须有新关键帧和音频，不能生成声称带声音的纯视频。
共享现有编码和音频，不重开设备、不转码；切编码先停止保存。
单调时间对齐只建立媒体起点，不校准采集/编码延迟，不证明长期口型同步。

NAS 必须是真实挂载且可写；失败不写空挂载目录、不回退本机。
路径逐级固定目录句柄，拒绝越界与链接；权限仍受 SMB 挂载和 NFS 服务器约束。
位置历史淘汰只删记录，不删文件；切目录不移动录像，重新选旧目录可再次列出。
已不存在的历史目录不自动创建；一处 NAS 故障不应阻塞其他位置。
空闲可录时长只从合适的完整历史文件估算，没有样本时隐藏。

采集重启、格式变化、断流、积压或空间不足会停止并报告原因，尽可能完成封装；
断电或封装失败保留未完成文件，可下载尝试恢复。应用正常退出先收尾。
NAS 内核 I/O 超时不意味着操作被取消，写入可能稍后完成；
资源回收在线程中保留 fd 至管线真正停止，资源耗尽则拒绝新录制。
强退不能保证 NAS 缓存持久化；无响应位置会提示遗漏，恢复后需重查。
正在录制的文件不可下载、删除；坏或缺失元数据不补造时长。
离线封装/解码测试入口在录制模块，仍未替代真实 HDMI 同步、长期录制和满盘测试。

## UU 远程

这是经网易云端供官方客户端使用的非官方集成；来源与许可见
[NOTICE](src/uu_remote/NOTICE)。登录代次、身份和续房见
[login](src/uu_remote/login.rs)、[cloud](src/uu_remote/cloud.rs)、
[signaling](src/uu_remote/signaling.rs)。
平台切换有独立身份，首次需扫码，切回恢复原身份；解绑只清当前平台登录。
二维码等同凭据，仅对认证网页可见；日志不得记录票据、令牌、SDP 或 TURN 凭据。
旧请求的失败不得覆盖新凭据，信令续房/重连不拆已建立桌面；
代理只影响显式允许的 REST/信令，不影响媒体。错误须区分连接、超时和解析，
不能把查询失败当二维码过期。

[peer](src/uu_remote/peer.rs)维护编码锁与角色：一个桌面控制者，新接管先松开旧按键、
停止旧媒体，旧工具连接可继续终端/文件；仅 data offer 须重新协商才可加视频。
视频任务结束可在原连接再请求桌面，音频存活不能掩盖视频结束。
固定编码不兼容拒绝，自动模式按共享编码锁回退；不能与要求 MJPEG 的服务并存。
客户端画质仅运行期有效，网页保存持久化。
Wave5 码率模式的自动画质根据匹配视频 SSRC 的新鲜 RR 调整码率，
REMB 提供包含该视频流的传输预算上限；静止低流量不用于向上探测。
调节保留分辨率、帧率和编码实例，共享观看者使用同一码率；手动设置退出自动调节。
厂商后端、HEVC 固定 QP/无损模式仍使用初始码率，不支持该闭环。
策略与时限见 [adaptation](src/uu_remote/adaptation.rs)；TWCC 仅计数，尚未接发送端带宽估算。
**待实板验收**：Mac 官方客户端反馈、运行期码率实际生效、弱网恢复与端到端延迟。
只有新设置产出帧才回复成功，重建管线影响全部共享观看者。

[输入映射](src/uu_remote/input.rs)按所选身份解释物理键；
Mac Fn、媒体及部分 JIS 键尚不支持，锁屏状态不可观测，不声明未实现能力。
绝对移动可合并，点击、释放、滚轮、键盘和相对移动形成顺序边界；
相对报告不得合并后截断，滚轮小增量保留余数，USB 加速不等于客户端像素滚动。
队列满须报错并松键，不能无限缓存；旧连接关闭后的迟到输入不得注入或松开新连接按键。
CONTROL 关闭需新会话，临时传输恢复则可恢复同一 Peer 输入。
HID 日志只含计数和耗时，USB 写完不等于主机已处理或画面已显示。

统计以 [发送窗口与接收反馈](src/uu_remote/video_metrics.rs)为准。
成功发送窗口不覆盖采集、编码内部等待、
网络交付及客户端解码显示；缓存关键帧可能早于当前订阅。
无 RR 不造零丢包，重复 NACK 不是唯一丢包或已重传数，RTT 估算受时钟变化影响。
过期或未知样本不能沿用旧会话；候选对含 relay 表示中继，但候选协议不能证明到 TURN 的协议，
实际收包接口仍须抓包核对。发送码率不含网络开销，也不代表客户端显示帧率。
历史验收见
[缩放记录](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/2026-10-24-uu-scaled-60fps.md)，
不能外推显示器共存、耐久、全部内容和网络；高刷待官方客户端实测。

### UU 文件传输（板子目录）

[文件实现](src/uu_remote/files/mod.rs)与
[存储约束](src/uu_remote/files/storage.rs)维护协议路径、限额和租约。
入口访问板子文件及虚拟镜像，HDMI/HID 无法读取被控机磁盘。
镜像须先卸载，挂载状态不明也拒绝读写；共享锁和实时 LUN 检查防止其他入口并发挂载。
只允许受支持镜像，目录拒绝穿越、链接与特殊文件。
未开放重命名/删除；取消或断开保留已收部分以便续传，未完成镜像不能当完整介质。
续传没有内容散列协商，须选同一来源文件。桌面角色变化不关闭 FILE 通道。

### UU 终端

[终端](src/uu_remote/terminal/mod.rs)、[串口](src/uu_remote/terminal/targets/serial.rs)、
[SSH](src/uu_remote/terminal/targets/ssh.rs)维护目标、限额和协议。
各目标须逐项开启；板子 shell 具有服务用户的维护权限。
串口独占，接管须确认；拔插尝试恢复，非阻塞写入放不下会丢弃并计数。
ACM 与其他功能共享端点，先按 [USB 配置](#hid-功能配置)调整组合。
SSH 使用独立密钥和 known_hosts，不读取用户 SSH 配置或代理；网页核对指纹后信任，
交互终端仍可能有 SSH 自身确认和口令提示。

DETACH/断线保留进程供重接回放，CLOSE/终止则回收；桌面替换不结束工具。
停用目标关闭对应会话，停用 UU、解绑、切身份或正常退出结束全部会话。
日志不记终端字节、SSH 地址、用户名、口令或密钥。
**未上板**：仍需官方各平台客户端验证入口、错误、重命名、保留进程重接和重绘，
以及 USB 枚举、串口权限、拔插、主机未开 ACM 时的读写和组合端点。

## 网页鉴权

运行配置见 [服务](deploy/vim4/one-kvm.service)；
托管模式只允许回环监听，由公共网关认证，内部账户保留而入口隐藏，
不能用运行中配置绕开监听、HTTPS 或鉴权模式限制。
CLI 覆盖不直接落库，后续保存使用有效配置；升级关闭旧数据库 HTTPS，
显式强开仍拒绝。原生模式保留上游账户行为。
应急访问用 SSH 隧道转发服务的回环端点，由 SSH 自身鉴权。

MCP、VNC、RTSP、RustDesk 与 UU 各自凭据仍生效，托管网页会话不能代替。
Redfish 不提供；流、WebSocket 和网页终端经过网关，Host/Origin/转发头由网关清理。
历史迁移见
[统一鉴权记录](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/2026-10-18-unified-auth-gateway.md)。

## MCP

配置从页面复制，接口和限额见 [认证](src/mcp/auth.rs)、[工具定义](src/mcp/definitions.rs)。
只接受独立 Key；网页会话不能代替 Key，Key 不能访问其他 API。
关闭保留 Key，重生成立即作废旧 Key；坐标基于截图，局部放大来自源图。
缺键盘或绝对鼠标须报错，不假成功。文本按 US 物理布局输入，先切英文输入法，
避免输入法改写；裸 Shift 可能切模式。自然滚动按被控机设置。

人工输入取消 MCP，仅释放自身按键；人工全部松开且空闲后才能恢复。
内置 Computer Use 与 MCP 互斥，入口当前隐藏，开关以
[可见性配置](web/src/composables/useFeatureVisibility.ts)为准。
活动含输入与截图，仅留内存，重启清空，不写系统日志。
截图从共享管线按需借帧，空闲不持 DMA，不另开采集、不请求关键帧；
厂商转换器单帧路径须固定帧率并先协商 caps，避免随机丢唯一帧。
验证见 [截图方法](docs/snapshot-validation.md)，主线 HDR 候选边界见 [HDR](#hdr-与画质)。

## 控制台与终端

统计见 [延迟诊断](#视频延迟诊断)。虚拟键盘区分单次与锁定修饰，关闭或后台松键；
快捷键录制不发向目标，只在浏览器保存。

板子终端关闭只隐藏会话，刷新或重连才新建。桌面侧栏与统计栏互斥，
移动端采用全屏布局。私有 ttyd 的适配以
[ttydClient](web/src/lib/ttydClient.ts)为准，按键行发原始输入，
不能用会加括号粘贴标记的 paste；断线统一从面板重连。
[浏览器验收](web/tests/terminal-panel.browser.mjs)拦截 HID 出站，需认证环境；
不以静态截图代替断线、尺寸和重连验证。
终端通过认证代理的私有 socket 访问，不启动发行版独立 ttyd 服务。

扩展程序与依赖锁由打包维护，不覆盖其他应用命令。只有所需网络扩展获得网络管理能力，
应用本身不获得该能力；日志不记录含密钥参数。
看门狗只向独立服务组开放，正常停用/切换走安全关闭；
不能强杀应用触发复位来代替部署验收，不修改 nowayout。

## 切换

管理器等待旧应用完全退出再启动所选应用，共享采集、编码与 USB，不可并发占用。
应用不重启 HDMI；ATX 未接线，不借用无关 GPIO。
音频使用稳定 card ID，旧数字编号须按真实声卡解析，不能把 USB 声卡误认 HDMI。
源切换与设备丢失独立处理，重开时真实故障仍告警；实现见
[音频采集](src/audio/capture.rs)和[设备解析](src/audio/device_linux.rs)。

音频诊断见 [diagnostics](src/audio/diagnostics.rs)：无样本不等于静音，
非零能量或重复块也不能单独证明硬件卡住；样本差计数不等于爆音，EAGAIN 不等于 XRUN。
不保存音频载荷；采样避开并行构建，结合接收端区分发送与播放问题。
历史与硬件验收只读
[上板索引](https://github.com/vim4-kvm/vim4-kvm/blob/main/docs/bringup/README.md)命中记录，
软件与旧证据不证明当前硬件通过。
