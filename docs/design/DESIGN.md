> **Historical design study (2026-10-15), written when the project was called ccrun.** [../whitepaper.md](../whitepaper.md) describes what vcontainer actually implements and is authoritative. Points superseded by the implementation: SNP is not implemented; the host's default configuration is trusted, so there is no ACPI allowlist, no BadAML patch and no RTMR0 appraisal; the kernel uses `KVM_GUEST=y` without `nopv`; `ip=dhcp` plus routes from the kernel log replace the DHCP client; `ccrun` is `vcontainer`.

# ccrun：GCP 上的最小机密容器运行时（TDX + SNP）设计稿 v0.3

> 状态：设计 v0.3（2026-10-15），未实现。v0.1（[DESIGN-v0.1.md](DESIGN-v0.1.md)）经过 Codex（[REVIEW-codex.md](REVIEW-codex.md)）和 Claude 四视角对抗审阅（每条都有反驳者复核），修订为 v0.2；Codex 第二轮审阅（[REVIEW-codex-r2.md](REVIEW-codex-r2.md)）之后再修订为 v0.3。逐条处置见 [REVIEW.md](REVIEW.md)。
> 事实依据：[FACTS-tdx.md](FACTS-tdx.md)、[FACTS-snp.md](FACTS-snp.md)。每条事实都标了已核实或推断，并附来源：源码行号、Google 文档，以及你本地 `~/vos-tdxroot` 和 `~/.cache/vos-build` 中的真机数据。

---

## 0. 一页摘要

- **形态**：只有一个可复现构建的 **PE 文件**，由 Linux EFI stub 内核、内置 initramfs 和内置 cmdline 组成；initramfs 里只有静态 Rust 二进制 `/init`。PE 放在一块只有 ESP 分区的 GPT 盘上，作为 `\EFI\BOOT\BOOTX64.EFI`，由 Google 固件直接加载。不需要 GRUB、shim、systemd-stub、rootfs 盘、containerd、runc 或 systemd。
- **一个镜像，三种模式**：

| `--tee` | 机型 | 拉取前状态的信任根 | 宿主恶意时满足要求 3？ |
|---|---|---|---|
| `tdx`（**推荐**） | C3 | Intel 硬件：MRTD（有 Google 签名背书）+ RTMR0–2 + RTMR3 账本 | **满足**。前提是信任 Google 固件二进制（有签名），以及宿主 ACPI（按审阅过的基线 pin，并打 BadAML 补丁） |
| `snp-svsm` | N2D，需要隐藏的 alpha 选项 `--svsm-config` | AMD 硬件：MEASUREMENT 覆盖 OVMF 和 SVSM；vTPM 运行在 SVSM 内，其 manifest v1 绑定在 REPORT_DATA 中 | **有条件满足**：M0 实测通过（§14），并信任 Google 签名的 SVSM 二进制 |
| `snp-hostvtpm` | N2D，GA | **整个 Google 宿主**（VMM 的 TIS 模拟 + vTPM） | **不满足**，只能防非宿主攻击者。policy 中不写 `allow_host_vtpm: true` 就不会 PASS |

  GCE 没有 direct kernel boot，也不能设置 HOST_DATA 或 ID block；SNP 没有 RTMR；GA 的 vTPM 运行在宿主侧。所以在 GA 的 SNP 上，恶意宿主场景下的要求 3 **做不到**。这是平台限制，ccrun 能做的是把它标清楚。
- **先验证、后投递（attest-then-launch）**：VM 启动后停在 `WAITING`，此时既没有 agent 镜像，也没有秘密。owner 的 CLI 先远程验证完整的拉取前状态：固件、内核、`/init`、宿主 ACPI，以及账本 `[boot, owner]`。验证通过后才发出 launch 包：config 明文，带 owner 签名；**只有秘密**用 HPKE 加密到本次启动的公钥。`/init` 收到后先把 launch 记入账本，进入 STARTING 并**停下**。CLI 再取一次 evidence，确认账本 `[boot, owner, launch(config)]` 与预期一致，然后发出 owner 签名的 commit。`/init` 收到 commit 后才按 digest 拉取 → 逐层校验并解包到 tmpfs → 执行。这样一来，**拉取前的完整状态（包括 config 的度量）在拉取开始前已被远端验证**，而不只是靠事后推断。这一步由 CLI 自动完成，用户无感。
- **隔离只做一件事**：不让 workload 伪造或破坏 ccrun 的证据。手段是 Docker 默认 capability 集合再减去 `MKNOD` 和 `SYS_CHROOT`，在子 mount namespace 里卸掉 init 的所有挂载，`/proc/sys` 只读，内核里没有 usermode helper、SysRq 和 coredump。不做沙箱。
- **代码量**：Rust 约 3.5k 行（guest 约 1.6k，CLI 和 verifier 约 1.6k，共享约 0.3k），脚本和配置约 280 行，另有 1 个约 150 行的内核补丁（BadAML）。对比：Confidential Space launcher 约 6.6k 行 Go（还要 containerd 和 COS），Tinfoil 约 16.8k 行 Go（还要 docker），dstack 约 133k 行 Rust。
- **使用**：一条幂等命令完成"建 VM → 验证拉取前状态 → 投递 → 验证运行态 → 打印入口"：`ccrun up NAME --tee tdx --image REG/REPO@sha256:… -p 8080 --secret KEY=@file`。第三方复核只需要一个自包含的 policy 文件：`ccrun verify URL --policy NAME.policy.json`。

---

## 1. 目标、非目标、威胁模型

### 1.1 目标
1. **G1 容器格式**：运行任意 linux/amd64 的 OCI/Docker 镜像：manifest、index 和 schema2 都支持，layer 支持 tar、gzip（含多成员）和 zstd，遵循 entrypoint、cmd、env、workdir、user 的语义，以及 OCI 的 whiteout 和替换语义。
2. **G2 最少代码、最易用**：一个 CLI、一个 init、一个构建脚本；部署和故障恢复用同一条幂等命令。
3. **G3 拉取前状态可验证**（v0.2 的精确表述）：在 agent 镜像的任何字节进入 VM 之前，verifier 能确认所有**决定"跑什么代码、用什么配置"的输入**，包括固件、内核、initramfs、cmdline、`/init`、宿主 ACPI（AML 本身就是代码）、owner 身份和 launch config。其余宿主输入分两类：要么由已度量的代码当作不可信数据解析（属于攻击面，见 §4.4 清单），要么只影响时间或可用性。
4. **G4 Rust**：guest 和 verifier 都用 Rust；guest 是静态 musl 二进制。C 代码只有 musl libc 和 `ring` 的密码学原语（C+asm），由版本钉死的构建容器编译。

### 1.2 非目标
- 不做 workload 沙箱：VM 就是边界，且单租户。D7 只做防伪造证据所需的那一点隔离。
- 不做：多容器编排、持久卷、live migration、GPU（A3 H100 留到 v2）、无人值守的秘密重投（v1 由幂等的 `ccrun up` 完成）。
- 不依赖 Google Cloud Attestation。验证全部在本地完成。

### 1.3 威胁模型
| 实体 | 假设 |
|---|---|
| Intel CPU + TDX module；AMD CPU + ASP | 可信，受 TCB/SVN 下限约束 |
| Intel PCS、AMD KDS | 可信（作为签名根） |
| Google 固件（闭源，二进制公开） | **可信，但前提是有 Google 签名背书**：MRTD 或 MEASUREMENT 必须出现在签名的内容中 |
| Google SVSM（`snp-svsm` 模式） | 可信，前提是它被背书中的 `svsm_measurement` 覆盖（二进制未公开） |
| Google 宿主：VMM、ACPI、TD HOB、NVRAM、设备模拟、metadata、DHCP、RTC、磁盘、网络、串口、键盘 | **不可信**。唯一例外是 `snp-hostvtpm`：此时整个宿主都必须可信，因为 vTPM 和承载全部 TPM 流量的 TIS MMIO 模拟都在宿主侧 |
| 镜像仓库、DNS | 不可信。完整性靠 digest；凭证机密性靠 WebPKI 加上一个时间下限（§6.3） |
| Owner | 可信，持有 Ed25519 owner key |
| Workload | 是否可信由 relying party 根据 digest 和 config 自行判断。ccrun 只保证"跑的就是它"，并保证它无法伪造 ccrun 的证据 |

攻击者想达成的目标有三个：(a) 让 VM 运行与声称不符的代码、配置或 ACPI；(b) 拿到 launch 时投递的秘密；(c) 伪造或篡改证据，包括谎报运行状态。可用性攻击不在范围内。

---

## 2. 关键设计决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | **单个 PE：内核 EFI stub + 内置 initramfs + `CONFIG_CMDLINE_OVERRIDE` + `noinitrd`；`CONFIG_MODULES=n`** | GCE 只会加载盘上的 `BOOTX64.EFI`，并把它的 **Authenticode** 度量进 RTMR1（TDX）或 vTPM 的 PCR4（SNP）。只要 TCG2 存在，EFI stub 就把外部 initrd 和 LoadOptions **只**度量进 vTPM，不进 RTMR2，所以所有内核输入都必须放在 PE 里面。NVRAM 由宿主控制（Boot####/BootNext 的 OptionalData），因此 `noinitrd` 必不可少：否则宿主可以通过 LoadFile2 或 `initrd=` 注入 initrd，覆盖 `/init`。LoadOptions 仍会被度量（只进 vTPM），但内核不会使用 |
| D2 | **不要 rootfs 盘，镜像放 tmpfs**；guest 永不写盘 | TDX/SNP 的私有内存本身提供机密性和完整性。GPT 是 RTMR1 中的一个事件，不写盘它就保持确定。内核也因此不编 NVMe、SCSI、ATA 驱动 |
| D3 | **TDX 的账本放 RTMR3**（Linux ≥6.16 的 sysfs）；**完全不用 vTPM 做度量** | 一个 quote 就覆盖全部。GCE TDX 的 vTPM 还有把 PE 记进 PCR2 的怪癖 |
| D4 | **SNP 的账本放 vTPM 的 PCR8**；三种模式共用代码（`snp-svsm` 与 `snp-hostvtpm` 只差约 100 行） | 平台限制见 §4.3；`snp-hostvtpm` 必须显式授权才能 PASS |
| D5 | **只提供 attest-then-launch 一种模式**；metadata 里只放 owner 公钥，且先度量再使用 | agent 几乎都需要秘密（API key），只有"先验证再投递"能保证秘密只交给验证过的那次启动。宿主没有 owner 私钥，无法替 owner 发起 launch；宿主若替换 metadata 里的公钥，账本 #1 就对不上。代价是重启后要重新投递，用同一条幂等的 `ccrun up` 即可。不需要秘密的场景可用 v1.1 的 metadata 自启动（约 40 行，§15） |
| D6 | **自写最小 OCI 拉取和解包**（`ureq`、只按 digest 拉、逐层流水线、在 chroot 子进程里解包） | 不需要 containerd/runc/youki（youki 约 23k 行，还带 C 依赖）。`ureq` 方案实测约 46 个 crate、1.8 MB；`oci-client` 方案是 156 个 crate、3.7 MB，还要 tokio |
| D7 | **只做防伪造证据的最小隔离**（细节见 §6.5）。内核：没有 usermode helper、SysRq、coredump、user ns、io_uring、BPF、userfaultfd。进程：新的 mount 和 pid ns，卸掉 init 的全部挂载，`/proc/sys` 只读，capability 为 Docker 默认集合减去 `MKNOD` 和 `SYS_CHROOT`，stdin 接 `/dev/null` | 账本只能追加、verifier 要求精确 replay，所以 workload **本来就改不了**"launch 的是哪份 config"。D7 防的是另外三件事：用任意 report_data 自铸 quote（借此伪造运行状态）；往账本追加事件，让证据永远验证失败；读取 init 的内存（里面有 registry 凭证） |
| D8 | **CLI 直接调用 `gcloud`** | 不必实现 GCE API 和 OAuth |
| D9 | **Secure Boot 关闭；PK/KEK/db/dbx 和 Boot 变量只做 digest 绑定，不评估取值** | RTMR1 已经 pin 住**实际加载**的 PE，而 PE 不读 LoadOptions；内核也不使用 UEFI 密钥（`LOAD_UEFI_KEYS=n`、没有模块、没有 kexec、`efi=noruntime`）。这些宿主可控的变量从构造上就影响不了安全。这样做也消除了 Google 默认 dbx 漂移带来的误报 |
| D10 | **内核用 `make allnoconfig KCONFIG_ALLCONFIG=kernel.miniconfig` 生成；构建后由独立的断言脚本逐项检查 must-be-on 和 must-be-off** | 审阅发现三个问题：defconfig 会带进几十个解析宿主设备的驱动；fragment 里写的 `=n` 会被 `select` 悄悄恢复（如 `ATA→SCSI`）；漏写依赖（如 `VIRT_DRIVERS`）时，`olddefconfig` 会悄悄丢掉 TDX/SNP 驱动 |
| D11 | **ACPI allowlist 来自经人工审阅的平台基线包；取消交互式 TOFU；BadAML 补丁为强制项** | RTMR0（或 SNP 的 PCR1）里的 ACPI digest 针对的是 guest 无法复原的 fw_cfg 原始 blob，而且会漂移。自动收录等于让宿主替你决定内核要执行哪段代码 |

---

## 3. 总体架构

```
 owner / relying party                          GCE（C3 TDX 或 N2D SNP）
┌──────────────────────────────┐   gcloud    ┌───────────────────────────────────────────────┐
│ ccrun（CLI + verifier）      │ ──────────▶ │ Google 固件（MRTD / SNP MEASUREMENT，签名背书）│
│  owner.ed25519               │             │  └ ESP:\EFI\BOOT\BOOTX64.EFI   ← 唯一的镜像   │
│  release.json（可复现）      │             │     内核 EFI stub + 内置 initramfs 和 cmdline │
│  platform.json（审阅基线）   │  HTTP :7700 │     └ /init（ccrun-init，Rust，静态）         │
│  NAME.policy.json（自包含）  │ ◀────────── │        ├ lo + DHCP + resolv.conf              │
│  Intel PCS / AMD KDS 缓存    │ /v1/evidence│        ├ metadata 中的 owner 公钥（先度量）   │
│  Google 背书缓存             │ ──────────▶ │        ├ 账本 → RTMR3 / vTPM PCR8             │
│                              │ /v1/launch  │        ├ 逐层：拉取→校验→解包（chroot 助手）  │
└──────────────────────────────┘             │        └ 启动助手：新 ns，卸载，exec 握手     │
                                             └───────────────────────────────────────────────┘
```

```
ccrun/
  crates/core/   # 账本、evidence/launch/config/status 的 schema、TCG2 日志解析与 replay、按事件类型绑定 digest
  crates/init/   # guest PID1（静态 musl）；--stage unpack / --stage run 为单线程助手
  crates/cli/    # up / down / verify / measure / golden / logs / keygen / image publish
  image/kernel.fragment       # 正向白名单（约 120 行）
  image/kernel.must-be-off    # 必须为 n 的符号（构建时断言）
  image/patches/0001-acpi-badaml-guard.patch   # 约 150 行，Flashbots、dstack、Contrast 均带此补丁
  image/build.sh              # 约 100 行
  image/Containerfile         # 钉死的构建环境：Rust、musl-gcc、内核工具链、mtools、gdisk
  platform/platform.json      # 维护者审阅并签名的平台基线
```

---

## 4. 启动与度量链

### 4.1 共同部分

内置 cmdline：`console=ttyS0 noinitrd nopv panic=-1 efi=noruntime random.trust_bootloader=off`
- `noinitrd`：理由见 D1。
- `nopv`：宿主可以通过 CPUID `0x4000_00xx` 自称 VMware 或 Hyper-V，触发对应的 PV 路径（tsc_khz 由宿主决定，steal-time 页处于解密状态）。`nopv` 关掉这些路径。
- `efi=noruntime`：不调用宿主固件的 runtime services。
- 不使用 `ip=dhcp`：GCE 下发 /32 掩码加 option 121 路由，而内核的 IP_PNP 不认 option 121。

initramfs 用 `gen_init_cpio` 生成，mtime 统一取 `KBUILD_BUILD_TIMESTAMP`：
```
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /run 0755 0 0
dir /etc 0755 0 0
dir /newroot 0755 0 0
file /init target/x86_64-unknown-linux-musl/release/ccrun-init 0755 0 0
```
TLS 根证书由 `webpki-roots` 编译进 `/init`。workload 的 rootfs 是挂在 `/newroot` 上的独立 tmpfs，**不要**挂在 `/run` 下面，否则 §6.5 卸载 init 的 `/run` 时会把它一起卸掉。

**内核**：Linux 6.18.y LTS。所需特性及其最早版本：tsm-mr sysfs（6.16）、TDX halt fix（6.15）、`cc_random_init`（6.9）、CCEL sysfs（6.4）。quote 长度修复 c3fd16c3b98e 已于 2026-04-02 回移到 6.18.y，build.sh 要断言内核版本不低于含该修复的版本。另需打一个**强制的 BadAML guard 补丁**：禁止 AML 的 SystemMemory OperationRegion 映射 System RAM。

**Kconfig**：`make allnoconfig KCONFIG_ALLCONFIG=image/kernel.miniconfig`。注意 `merge_config.sh -s` 对"请求值 ≠ 生效值"只告警、不失败，Codex 第二轮指出了这一点。所以要另写一个约 15 行的断言脚本：把 miniconfig 中每个 `=y`，以及 must-be-off 清单中的每一项，和最终 `.config` 逐项比对，不一致就失败。下面只列要点；完整的 miniconfig 以 M0 在 C3 和 N2D 上能启动为准。
- **fragment（正向白名单）**：`64BIT SMP MULTIUSER SHMEM POSIX_TIMERS PROC_SYSCTL FILE_LOCKING PRINTK HYPERVISOR_GUEST PARAVIRT X86_X2APIC ACPI EFI EFI_STUB PCI PCI_MSI VIRT_DRIVERS INTEL_TDX_GUEST TDX_GUEST_DRIVER AMD_MEM_ENCRYPT SEV_GUEST TSM_REPORTS CONFIGFS_FS NET INET UNIX PACKET NETDEVICES ETHERNET NET_VENDOR_GOOGLE GVE TCG_TPM TCG_TIS TCG_CRB TCG_SVSM SECURITYFS BLK_DEV_INITRD INITRAMFS_COMPRESSION_NONE TMPFS TMPFS_XATTR DEVTMPFS PROC_FS SYSFS SERIAL_8250 SERIAL_8250_CONSOLE TTY UNIX98_PTYS BINFMT_ELF BINFMT_SCRIPT FUTEX EPOLL NAMESPACES PID_NS EXPERT STATIC_USERMODEHELPER`，并设 `STATIC_USERMODEHELPER_PATH=""`。这样会禁用**所有** usermode helper，包括 core_pattern 管道和 request_module。
- **must-be-off（构建时断言）**：`MODULES KEXEC KEXEC_FILE DEVMEM MAGIC_SYSRQ COREDUMP USER_NS IO_URING BPF_SYSCALL USERFAULTFD VT INPUT_KEYBOARD SERIO DMI KVM_GUEST HW_RANDOM_TPM TCG_TPM2_HMAC LOAD_UEFI_KEYS ATA SCSI BLK_DEV_NVME USB_SUPPORT DRM SOUND VIRTIO HOTPLUG_PCI KPROBES HIBERNATION IA32_EMULATION DEBUG_FS XEN JAILHOUSE_GUEST ACRN_GUEST BHYVE_GUEST`。最后 4 个在 `nopv` 下仍会做 hypervisor 检测（`ignore_nopv`），所以必须关掉（已核实 v6.18 `arch/x86/kernel/cpu/hypervisor.c:71`）。
- `KVM_GUEST=n` 在 TDX 上没有问题，因为 TSC 被强制视为可靠。但在 N2D SNP 上，去掉 kvmclock 后的时钟源**从未实测**，列入 M0。若有问题，就只为 SNP 打开它，代价是多一个宿主共享页。

### 4.2 TDX（C3）

| 寄存器 | 内容（GCE 实测） | verifier 判定 |
|---|---|---|
| MRTD | Google TDVF | 从 `gce_tcb_integrity/ovmf_x64_csm/tdx/<MRTD>.binarypb` 取背书，先验 RSA-PSS-SHA256 签名：`GCE-uefi-signer` → 钉死的 `GCE-cc-tcb-root_1`。**钉 root，不钉 signer**，signer 每月轮换。签名通过后，MRTD 必须**出现在签名内容 `tdx.measurements[*].mrtd` 中**，且 `svn ≥ 下限`。在当前固件上，MRTD 与内存大小无关 |
| RTMR0 | `TdxTable`（TD HOB）、`Fv(CFV)`、SecureBoot/PK/KEK/db/dbx、SEP、3×`ACPI DATA`、BootOrder/Boot####，以及（开 Secure Boot 时的）Authority | **逐事件按语法检查**（§9.2）。TD HOB、CFV 和 ACPI 三元组必须在 `platform.json` 中；其余事件只做 digest 绑定（D9） |
| RTMR1 | `Calling EFI App`、SEP、`GPT`、**`BOOT_SERVICES_APPLICATION` = PE 的 Authenticode SHA-384**、[可选 1 个 `EBS Failed`]、`EBS Invocation`、`EBS Success` | **语法 + 精确值**：GPT 和 App 必须等于 `release.json`；不允许出现任何其他应用或驱动事件。`release.json` 同时给出两条合法轨迹的 RTMR1 值：正常轨迹，以及 EDK2 在 ExitBootServices 重试时多出一个 `EBS Failed` 的轨迹 |
| RTMR2 | 预期全零：stub 的 PCR9 事件都进了 vTPM | 精确等于 0（**待 M0 确认**） |
| RTMR3 | ccrun 账本 | replay 结果完全相等，语法见 §5 |
| MROWNER | GCE 写入的 `SHA-384({"instanceId":…,"numericalProjectId":…,"zone":"…"})` | **`ccrun up` 强制检查**，并写进 policy；`verify` 在 policy 中有此项时也检查。它只能防非 Google 的攻击者，比如有人用你公开的 owner key 起一个 TD 来劫持 launch |
| MRCONFIGID / MROWNERCONFIG / MRSERVICETD | 0 | 必须为 0。若出现 v5 quote，也要求 MRSERVICETD 为 0 |
| TD_ATTRIBUTES / XFAM | `0x10000000` / `0x602e7` | 精确 pin |
| REPORTDATA | `SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status))` | 同时绑定新鲜度和运行状态（§7.1） |

### 4.3 SNP（N2D Milan）

**平台事实**：GCE SNP 的 MEASUREMENT 只覆盖 Google OVMF 和 VMSA。实测 11 个不同镜像、1127 次观测都是同一个值。所以 PE 的身份只能从 vTPM 的 PCR4 得到，而 vTPM 运行在哪里，决定了这个身份是否可信：

| | `snp-hostvtpm`（GA） | `snp-svsm`（alpha：`gcloud alpha … --svsm-config tpm=EPHEMERAL,snp-irq=UNRESTRICTED`，镜像需声明 `SNP_SVSM_CAPABLE`） |
|---|---|---|
| vTPM 所在位置 | 宿主侧的软件实现（BoringSSL）。guest 经 TIS MMIO 访问，这些访问由 VMM 通过 #VC/GHCB 模拟 | SVSM 内（VMPL0，处于 TEE 内部）；guest 运行在 VMPL2 |
| PE 身份的信任根 | **整个 Google 宿主**：VMM 可以在传输途中改写固件的 PCR_Extend，也可以直接冒充 vTPM | AMD 硬件 + Google 签名的 SVSM |
| SNP report 能证明什么 | 只能证明"这是真 SNP VM，跑的是 Google 固件"。面对恶意 VMM，它**对代码身份毫无帮助**：任何内核都能拿到真 report，并填任意 REPORT_DATA | SVSM 中 vTPM 的 manifest v1（含 AK）由硬件绑定 |
| verdict | `root=amd+google-host`。**policy 不写 `allow_host_vtpm: true` 就 FAIL**；`ccrun up --tee snp-hostvtpm` 会打印醒目警告 | `root=amd+google-svsm` |

**两种模式共用的 PCR 规则**：SHA-256 bank；quote 覆盖 PCR0–9。固件事件日志 replay 覆盖 PCR0–7 和 PCR9；PCR8 单独用账本 replay。

| PCR | 内容 | 规则 |
|---|---|---|
| 0 | `S-CRTM_VERSION "GCE Virtual Firmware v2"`、`NONHOST_INFO`（tech=4 表示 SNP）、SEP；另有 SP800-155 的 `NO_ACTION` 记录（不参与 extend） | 检查语法；S-CRTM 和 NonHostInfo 都要求 `H(data)==digest`，NonHostInfo 的取值必须是 SNP |
| 1 | 3×`ACPI DATA`、BootOrder/Boot####、SEP | **ACPI 三元组（SHA-256）必须在 `platform.json[snp][machine-type]` 中**；Boot 变量只做 digest 绑定（D9）。也就是说，**SNP 同样需要审阅过的基线** |
| 2、3、6 | 只有 SEP | **精确等于"只有 SEP"时的常量**（`3d458cfe…`）。否则宿主可以通过 NVRAM 的 `Driver####` 或 PCI option ROM 加载 UEFI 驱动；这类驱动只记在 PCR2，却能在 Authenticode 度量之后篡改内核 |
| 4 | `Calling EFI App`、SEP、`BOOT_SERVICES_APPLICATION`（PE 的 Authenticode SHA-256） | 精确等于 `release.json` |
| 5 | [SEP]、GPT、[EBS Failed]、EBS Invocation、EBS Success | GPT 等于 release。SEP 是否出现、位置在哪，以 M0 实测为准，然后固化进 release.json 的两条轨迹 |
| 7 | SecureBoot/PK/KEK/db/dbx、SEP，以及（开 Secure Boot 时的）Authority | 只做 digest 绑定（D9） |
| 9 | stub 的 `EV_EVENT_TAG`（LoadOptions） | **不评估**，最多允许 1 个事件。tag 标签不在 digest 覆盖范围内，凭它证明不了任何事；真正阻止 initrd 注入的，是钉死的 PE 中内置的 `noinitrd`（M0 做对抗测试） |
| 8 | ccrun 账本 | 用账本 replay，必须完全相等 |

**report 与新鲜度**：SNP 的 guest request 一旦失败，内核就会清零 VMPCK，之后无法再取 report。所以**每次启动只取一次 report**，时机在账本事件 0 之后、开放 HTTP 之前；取失败就关机，不重试。
- `snp-hostvtpm`：`REPORT_DATA = SHA-512("ccrun/v1/snp-boot\0" ‖ boot_id ‖ SHA-256(AK TPMT_PUBLIC))`。
- `snp-svsm`：通过 configfs-tsm 做 **service attestation**：
  - `service_provider=svsm`；
  - `service_guid=c476f1eb-0123-45a5-9641-b4e7dde5bfe3`；
  - **`service_manifest_version=1`**（默认值 0 的 manifest 只含 EK，绑不住 AK）；
  - `inblob = SHA-512("ccrun/v1/snp-boot\0" ‖ boot_id)`；
  - 得到 `REPORT_DATA = SHA-512(inblob ‖ manifest)`。
- 新鲜度：每个请求取一次新的 **TPM quote**，`qualifyingData = SHA-256("ccrun/v1/quote\0" ‖ nonce ‖ SHA-256(report) ‖ SHA-384(status))`。verifier 用的 `boot_id` 取自 `ledger[0]`，而不是 JSON 字段。

**guest 后端**：只用 configfs-tsm 文件 I/O 和手写的 TPM2 命令，不依赖 `tss-esapi` 或 libtss2，也不依赖 `sev` crate。
1. 取 report：使用 `privlevel_floor`，不写死 VMPL0。保存 `outblob`；`auxblob` 只用于调试。
2. TPM（`/dev/tpmrm0`）：
   - `NV_ReadPublic` 和 `NV_Read`（owner auth、空密码、分块读）读取 AK 模板（`0x01c10001`）；**两种模式都要读**，因为 SVSM 也把 AK 模板写在 Google 的 NV index 中，具体 index 待 M0 确认。只有 `snp-hostvtpm` 另外读取 AK 证书（`0x01c10000`）。`snp-svsm` 没有带实例身份的证书，policy 中的实例绑定在这个模式下为空。
   - `CreatePrimary`（Endorsement 层级）。`snp-hostvtpm` 要求生成密钥的模数等于证书中的公钥；`snp-svsm` 要求 TPMT_PUBLIC 等于 manifest v1 中的某一项，且带 `restricted|sign|fixedTPM` 属性。
   - `PCR_Extend(8)`、`Quote`（NULL scheme）、`FlushContext`。
   研究阶段写过一个 132 行、只用 std 的草稿，能编译，但**没在真 TPM 上跑过**。
3. 事件日志：挂载 securityfs，读取 `/sys/kernel/security/tpm0/binary_bios_measurements`，与 CCEL 共用同一个 TCG2 解析器。

### 4.4 拉取前输入清单（以 TDX 为例；SNP 同理，把 RTMR 换成 PCR）

| 输入 | 来源 | 处理方式 |
|---|---|---|
| 固件代码 | Google | MRTD 在签名背书的内容中 |
| PE（内核、initramfs、cmdline、`/init`） | ccrun 发布 | RTMR1 精确（Authenticode） |
| GPT | 盘（宿主提供） | RTMR1 精确；guest 永不写盘 |
| ACPI 表（AML 就是代码） | 宿主 | RTMR0 必须在审阅过的基线中；BadAML 补丁限制 AML 访问 RAM |
| TD HOB、CFV | 宿主、固件 | RTMR0 必须在基线中（v2 改为从背书的 `.fd` 离线推导） |
| NVRAM：Boot####、BootNext、Driver####、Secure Boot 变量、LoadOptions | 宿主 | **构造上无害**：RTMR1 pin 住实际加载的 PE，且不允许任何驱动事件；PE 忽略 LoadOptions，不加载 initrd；内核不读 UEFI 密钥 |
| CPUID（含 0x4000_00xx） | TDX module 和宿主 | 安全相关部分由 XFAM 和 attributes 覆盖；PV 叶子由 `nopv` 屏蔽 |
| SMBIOS/DMI | 宿主（未度量） | `DMI=n`，内核不解析 |
| 串口输入、PS/2 键盘 | 宿主 | `MAGIC_SYSRQ=n VT=n INPUT_KEYBOARD=n`；workload 的 stdin 接 `/dev/null`。**v0.1 中这是真漏洞**：宿主能用 SysRq 让内核把 GPR dump 到串口 |
| 设备的 MMIO/PCI（gVNIC、vTPM 的 TIS、UART、RTC） | 宿主 | **攻击面**：由已度量的驱动当作不可信数据解析；驱动集合已压到最小（D10） |
| DHCP 应答 | 宿主 | 由 `/init` 中的有界解析器处理，只影响网络可用性；hostname 不取自 DHCP |
| metadata（`ccrun-owner`） | 宿主 | 先度量为账本事件 1，再使用 |
| RTC 时间 | 宿主 | 只用于 TLS 有效期判断，并以 owner 签名的 `issued_at` 作为下限（§6.3） |
| 随机数 | RDRAND/RDSEED | 可信；`HW_RANDOM_TPM=n` |
| launch config | owner | 账本事件 2，有签名 |
| 秘密 | owner | HPKE 加密；只把密文的 hash 记入账本事件 2（便于事后审计） |
| CCEL 和事件日志 | 固件内存 | 本身不可信；靠 replay 和按事件类型绑定 digest 来认证 |

---

## 5. 运行时账本

编码沿用 dstack v1，现有工具可以直接 replay：
- `preimage = u32le(0x08000001) ‖ ":" ‖ name ‖ ":" ‖ payload`
- TDX：`RTMR3 ← extend(SHA-384(preimage))`，即向 `…/tdx_guest/measurements/rtmr3:sha384` 写入 48 字节。
- SNP：`PCR8 ← extend(SHA-256(preimage))`。

| # | name | payload | 时机 |
|---|---|---|---|
| 0 | `ccrun.boot` | `ver ‖ boot_id(16) ‖ hpke_pk(32)` | 启动后，任何网络 I/O 之前 |
| 1 | `ccrun.owner` | metadata 中 `ccrun-owner` 的**原始字节** | 读到后立即记录（先度量后解析） |
| 2 | `ccrun.launch` | `config 原始字节 ‖ SHA-512(enc‖ct)` | 签名和 HPKE 验证通过、CAS 抢到之后，**发出任何 registry 请求之前** |
| 3 | `ccrun.exec` | 解析出的平台 manifest digest | 解包完成后、execve 之前（封口） |

规则：
- 状态与账本前缀必须对应：

| 状态 | 账本 |
|---|---|
| WAITING | `[0,1]` |
| STARTING、PULLING | `[0,1,2]` |
| RUNNING、EXITED | `[0,1,2,3]` |
| FAILED | `[0,1,2]` 或 `[0,1,2,3]` |

- **一致性快照**：extend 操作、状态变化、生成 evidence 都持有同一把锁。evidence 处理函数在锁内读取 status 和账本，取 quote，再释放锁。这样 status、账本和 quote 三者总是对应同一时刻。
- verifier 从全零开始 replay，必须完全相等。因此 init 不再自查寄存器初值（v0.2 删掉了这一步，也省掉了 TPM 的 PCR_Read）。
- 账本只能追加，又要求完全相等，所以**任何人都无法让 verifier 接受另一份 config**，最多只能让它 FAIL。

---

## 6. `ccrun-init`

### 6.1 状态机
```
BOOT ─► WAITING ─(launch, CAS)─► STARTING ─(commit)─► PULLING ─► RUNNING ─► EXITED
  │                         └──────── 任何失败 ──► FAILED（终态；仍提供 evidence，不重试）
  └─ 失败 ─► 串口打印 "ccrun: FAIL <step>: <err>" ─► 关机
```
`status`（`{"state","exit_code","error"}` 的 canonical JSON）被绑定进 REPORTDATA（TDX）或 qualifyingData（SNP）。

### 6.2 BOOT
1. 挂载 proc、sys、devtmpfs、`/run`（tmpfs）和 configfs；SNP 还要挂 securityfs。设置 `PR_SET_DUMPABLE=0`。PID1 的 fd 0、1、2 改指 `/dev/null`，只在需要打印时临时打开 `/dev/console`。
2. 通过 configfs-tsm 的 `provider` 判断 TEE 类型（`tdx_guest` 或 `sev_guest`）。生成 `boot_id` 和 HPKE X25519 密钥对，extend 事件 0。SNP 在这一步取那唯一一次 report。
3. 网络：
   - 用 netlink 把 `lo` 设为 UP；
   - 跑 DHCP（`dhcproto`，UDP + `SO_BINDTODEVICE`，**设置 BROADCAST 标志**），配置地址、MTU（option 26）和 option 121 路由；
   - 写 PID1 自己的 `/etc/resolv.conf`（来自 option 6）；
   - hostname 固定为 `ccrun`。
   v1 不做 T1 续租；若 M0 发现 GCE 会在租约到期后收回地址，再补上。
4. 读取 `http://169.254.169.254/computeMetadata/v1/instance/attributes/ccrun-owner`（带 `Metadata-Flavor: Google` 头），拿到原始字节先 extend 事件 1，再解析。
5. 在 `:7700` 起 HTTP 服务（`std::net` + `httparse`）：每个连接一个线程，有读超时和 body 上限；`/v1/evidence` 并发为 1，并限速。进入 WAITING。

### 6.3 STARTING 和 PULLING
1. 收到 `POST /v1/launch` 后，先做**没有副作用**的检查：验证签名、HPKE 解封。然后在互斥锁下 CAS WAITING→STARTING。只有赢家继续，其余请求一律返回 409。
2. **时间下限**：若 RTC 早于 `issued_at`（在 owner 签名覆盖范围内），就用 `clock_settime` 把系统时间设为 `issued_at`。之后所有 TLS 证书校验看到的时间都 ≥ `issued_at`，宿主无法把时钟回拨到某张过期证书的有效期内。
3. extend 事件 2，进入 STARTING，等待 commit。
4. 收到 `POST /v1/commit`：`sig = Ed25519(owner_sk, SHA-512("ccrun/v1/commit\0" ‖ boot_id ‖ SHA-384(ledger[0..=2])))`。验签通过后进入 PULLING。CLI 在发 commit 之前，已经用一份新的 evidence 验证了 STARTING 状态。
5. **逐层流水线**：下载 → 边下载边算 sha256，读到 `descriptor.size + 1` 字节就中止 → 校验 → 交给解包助手 → 删除 blob。内存峰值为 rootfs 加上最大的一层。
6. 凭证：
   - 只接受 owner 投递的**短期 token**（如 `gcloud auth print-access-token` 的输出）；
   - 只发给授权的**完整 HTTPS origin**（scheme、host 和 port 都要匹配）：即 `image` 所在 registry 的 origin，以及 config 中显式列出的 token realm origin（`registry_auth.realm_origins`）；
   - 重定向时不带 Authorization（ureq 默认如此）。
   - 若选 `vm-service-account`，`ccrun up` 必须显式指定一个只有 Artifact Registry 读权限的服务账号。其余模式一律 `--no-service-account --no-scopes`。

### 6.4 解包助手（`/init --stage unpack`，单线程，先 chroot 到 `/newroot`）
- 在 chroot 内，绝对路径的 symlink 只会在 rootfs 内部解析，例如 `/var/run -> /run`。
- **每层分两遍**：第一遍只处理 `.wh.*` 和 `.wh..wh..opq`，且只作用于下层；第二遍处理其余条目。
- **替换语义**：对每个条目先 `lstat`；除非新旧都是目录，否则先 `remove_dir_all` 或 `remove_file`，再 `unpack_in`。这样能正确处理硬链接被重新发布、目录被文件替换等情况。
- 跳过 char 和 block 设备节点；保留 FIFO、uid、gid、mode 和 xattr（含 `security.capability`）。
- gzip 用 `flate2::MultiGzDecoder`（eStargz 是多成员 gzip），zstd 用 `ruzstd`。tar 读完后要把解压器**读到 EOF**，并要求压缩 blob 被完整消费，有残余就失败。否则 Docker 和 ccrun 看到的 rootfs 可能不同。
- 写入 `/etc/resolv.conf` 和 `/etc/hosts`（含 `127.0.0.1 localhost` 和 `::1 localhost`）。秘密写到 `/run/secrets/<name>`：先 chown 给 workload 的 uid，再设为 0400。
- 完成后由 PID1 extend 事件 3。

### 6.5 启动助手（`/init --stage run`，单线程）与 D7
1. 助手执行 `unshare(CLONE_NEWNS|CLONE_NEWPID)`，然后 fork；子进程成为新 pid ns 中的 1 号进程。
2. 子进程在仍有特权时依次执行：
   1. `mount --make-rprivate /`；
   2. 在 rootfs 中挂载新的 proc：`/proc/sys`、`/proc/sysrq-trigger`、`/proc/irq`、`/proc/bus`、`/proc/fs` 重新挂为只读；`/proc/kcore`、`/proc/keys`、`/proc/timer_list` 用空文件遮蔽（与 Docker 的 readonly/masked paths 相同）。另挂只读 sysfs，以及 `/dev` tmpfs（只含 null/zero/full/random/urandom/tty，外加 devpts 和 shm）；
   3. **对 init 的 `/proc`、`/sys`（连带 configfs 和 securityfs）、`/dev` 和 `/run` 执行 `umount2(MNT_DETACH)`**；
   4. `chdir("/newroot")`，把它 `MS_MOVE` 到 `/`，再 `chroot(".")`（switch_root 的做法；内核 ≥7.0 时可改用 pivot_root）。第 2 步的挂载点都在 `/newroot/...` 下，第 3 步卸载的是 init 自己的挂载点，两者互不影响；
   5. capability：bounding 集合 = Docker 默认集合减去 `MKNOD` 和 `SYS_CHROOT`；清空 inheritable 和 ambient；
   6. setgroups、setgid、setuid，然后 chdir；
   7. `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`：只把其余 fd **标记**为 CLOEXEC，不关闭它们，握手管道因此能保留到 execve；stdin 接 `/dev/null`；stdout 和 stderr 按 `config.logs` 接到串口或 `/dev/null`；
   8. **exec 握手**：第 1 到 7 步中任何一步失败，子进程都把 `(step, errno)` 写进握手管道，再 `_exit`；之后执行 execve。
3. 助手的判定：
   - 读到 `(step, errno)`：FAILED。
   - 读到 EOF：说明 execve 已越过 point of no return，状态转为 RUNNING。注意此后 workload 仍可能立刻退出，所以 **status 中的 `running` 只表示"execve 已提交，并且在取 quote 的那一刻进程还活着"**。evidence 处理函数在一致性锁内调用 `waitpid(WNOHANG)` 确认。
   - 助手负责 reap workload，并把退出码交给 PID1。

最终效果：workload 看不到 configfs、可写 sysfs、TEE 或 TPM 设备、`/proc/1` 和 `core_pattern`；内核里也不存在 usermode helper、SysRq、user ns、io_uring 和 BPF。它仍然能访问 `localhost:7700`，那里只有只读的 evidence 接口，以及启动后恒返回 409 的 launch 接口。这两个接口的请求解析是攻击面，因此保持极小。

---

## 7. 协议

协议走明文 HTTP；安全性来自签名、HPKE、quote 和 status 绑定。

### 7.1 `GET /v1/evidence?nonce=<64 hex>`
```json
{ "v": 1, "tee": "tdx|snp-svsm|snp-hostvtpm",
  "status": "<b64 canonical JSON: {state, exit_code, error}>",
  "ledger": [{"name": "...", "payload": "<b64>"}],
  "tdx": { "quote": "<b64>", "ccel": "<b64，已去掉 0xFF 填充>" },
  "snp": { "report": "<b64>", "manifest": "<b64>|null", "tpm_quote": {"attest": "<b64>", "sig": "<b64>"},
           "ak_public": "<b64>", "ak_cert": "<b64>|null", "eventlog": "<b64>" } }
```
- TDX：`REPORTDATA = SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status))`，每个请求取一次 quote。SNP 的绑定方式见 4.3。
- 可选参数 `&acpi=1`：附带 `/sys/firmware/acpi/tables/*` 的原文，只给 `golden` 审阅用。绑定方式：先构造 `acpi_manifest`，即按文件名排序的 `(name, SHA-384(content))` 列表；TDX 用 `REPORTDATA = SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status) ‖ SHA-384(acpi_manifest))`；SNP 则把 `SHA-384(acpi_manifest)` 附加到 TPM quote 的 qualifyingData 里。这只是**尽力而为的证据**：这些表是在 AML 执行之后读出的，与 RTMR0 中 pre-patch 的 digest 之间**没有密码学关联**（见 §13.1）。

### 7.2 `POST /v1/launch`
```json
{ "boot_id": "...", "issued_at": 1792000000, "config": "<b64>",
  "sealed": {"enc": "<b64>", "ct": "<b64>"} | null, "sig": "<b64>" }
```
- `sig = Ed25519(owner_sk, SHA-512("ccrun/v1/launch\0" ‖ boot_id ‖ hpke_pk ‖ u64(issued_at) ‖ SHA-512(config) ‖ SHA-512(enc‖ct)))`
- HPKE 采用 RFC 9180 base mode，套件为 X25519 / HKDF-SHA256 / ChaCha20-Poly1305，`info = "ccrun/v1/secrets\0" ‖ boot_id`。
- **config 是公开的**：线上明文传输，之后还会通过账本事件 2 返回给任何能访问 `:7700` 的人。所以 env 里只能放非机密配置，机密一律走 `--secret`。

### 7.3 `POST /v1/commit`
```json
{ "boot_id": "...", "ledger_sha384": "<hex>", "sig": "<b64>" }
```
只在 STARTING 状态下接受，且只接受一次。

### 7.4 launch config（被度量的那一份）
```json
{ "image": "us-docker.pkg.dev/p/r/agent@sha256:<64hex>",
  "entrypoint": null, "cmd": null, "env": ["MODEL=..."], "workdir": null, "user": null,
  "secrets": ["OPENAI_API_KEY"],
  "registry_auth": {"mode": "none|sealed-token|vm-service-account", "realm_origins": ["https://us-docker.pkg.dev"]},
  "logs": "serial|none",
  "tls_spki_sha256": null }
```
- 解析时 `deny_unknown_fields`，且**只接受带 digest 的镜像引用**。
- `tls_spki_sha256` 用于应用通道绑定：owner 用 `--secret` 投递 workload 的 TLS 私钥，同时把对应公钥的 SPKI hash 写进 config，这个 hash 因此被度量。`ccrun verify` 会打印它，客户端据此 pin 住 workload 的证书。
- 为什么需要它：否则宿主可以一边在 `:7700` 转发真实的 evidence，一边把 `:8080` 换成另一个服务。

---

## 8. 容器语义（与 `docker run` 的差异）

支持：
- OCI 和 Docker 的 manifest 与 index（选取 linux/amd64）；
- layer 格式：tar、gzip（支持多成员）、zstd（只支持单帧，多帧会被拒绝）；
- entrypoint、cmd、env、workdir、user；
- 两种 whiteout、替换语义、文件 capability。

与 `docker run` 的差异：
- 网络相当于 `--net=host`，`lo` 可用；
- 没有 cgroup 和 seccomp；
- capability 少了 `MKNOD` 和 `SYS_CHROOT`；
- 没有 user ns，所以 bubblewrap、Chrome 沙箱这类程序会失败；
- 没有 io_uring；
- stdin 是 `/dev/null`；
- 只能运行一个容器，rootfs 在 RAM 中。

---

## 9. Verifier

### 9.1 输入
- **`release.json`**：由 `ccrun measure` 生成，包括 PE 的 Authenticode SHA-384 和 SHA-256、GPT 事件的 digest（两个 bank），以及预期的 RTMR1/RTMR2 和 PCR4、PCR5、PCR2/3/6。
- **`platform.json`**：维护者审阅并签名的基线，包括：
  - `tdx[MRTD] = {cfv, td_hob[machine-type]}`；
  - `acpi[tdx|snp][machine-type] = [{digests, iasl_dump_sha256, reviewed_by, date}]`；
  - TCB 下限。

  用户也可以用 `ccrun golden` 生成提案，审阅后用自己的 key 签名。
- **`NAME.policy.json`**：由 `ccrun up` 写出，**自包含**，第三方有它就够了。内容包括：
  - `tee`（**必填**；evidence 的模式必须与之一致，防止降级）；
  - `machine_type`；
  - owner 公钥和 config 原始字节；
  - **发布身份**：从 release.json 拷贝 PE 和 GPT 的 digest（两个 bank）、两条 RTMR1 轨迹、PCR2/3/6/4/5 的期望值，以及 release 版本号；
  - 实例绑定（TDX 用 PZID；`snp-hostvtpm` 用 AK 证书中的身份；`snp-svsm` 为空）；
  - 从 platform.json 拷贝来的相关基线条目；
  - TCB 下限、`allow_host_vtpm`、可选的 `tls_spki_sha256`。

### 9.2 TDX
1. 用 `dcap-qvl =0.7.0` 验证 quote：`default-features=false`，启用 `[std, rustcrypto, default-x509]`（共 101 个 crate），离线运行；不开 `danger-allow-tcb-override`。`verify()` 只负责密码学校验和计算 TCB 状态，**策略要由我们自己做**（Codex 第二轮指出 v0.2 漏了这一步）：
   - collateral 取 Intel PCS v4 的最新 standard 版本，要求 `tcbEvaluationDataNumber ≥ min_tcb_eval_num`，并检查 `nextUpdate > now`。`now` 取 verifier 本机时间，这是 relying party 自己信任的时间；
   - 平台、TDX module 和 QE 的状态都必须在 `allowed_status` 中，默认只有 `UpToDate`；
   - advisories 必须 ⊆ `allowed_advisories`；
   - Intel 发布 TCB recovery 后、Google 完成升级前，可以在 policy 中设一个带截止日期的 `tcb_grace_until`，期间 OutOfDate 只作警告。不再采用 v0.2 的"按旧 eval 版本取 collateral"：旧版本终究会被 PCS 以 HTTP 410 下架。
2. 检查 REPORTDATA（nonce + status）、attributes、XFAM、MRCONFIGID/MROWNERCONFIG/MRSERVICETD，以及 MROWNER == PZID。
3. MRTD 必须出现在背书的签名内容中（§4.2）。
4. **解析 CCEL**：有界解析，只用 SHA-384 bank，`0xFF` 只能作为末尾填充。index 为 0 的 `EV_NO_ACTION` 记录（Spec ID 头、SP800-155 Event3）单独处理：限制长度和条数，不参与 replay。其余事件的 MR index 只允许 1..3。replay 结果必须等于 RTMR0..2。
5. **按事件类型绑定 digest**：
   - `H(data)==digest`：SEPARATOR、EFI_ACTION、GPT、VAR_DRIVER_CONFIG、VAR_AUTHORITY；SNP 另有 S_CRTM_VERSION、NONHOST_INFO；
   - `H(VariableData)==digest`：VAR_BOOT（GCE 的怪癖）；
   - 不透明比较，忽略事件数据：HANDOFF_TABLES2（TD HOB）、FIRMWARE_BLOB2（CFV）、ACPI DATA（PLATFORM_CONFIG_FLAGS）、BOOT_SERVICES_APPLICATION；SNP 另有 PCR9 的 EVENT_TAG（不评估，最多 1 个）。
   然后按 §4.2 的语法检查；出现未列出的事件类型或多余事件，一律 FAIL。
6. RTMR2 == 0。RTMR3 == 账本 replay，且符合 §5 的语法。`ledger[1]` == policy 中的 owner 公钥；账本中存在 #2 时，其中的 config 必须等于 policy 中的 config。账本前缀必须与 status 中的状态对应（§5）。
7. 输出 PASS，并列出：每条 claim、`root=intel`，以及剩余假设清单（Google 固件二进制、ACPI 基线的审阅者和日期等）。

### 9.3 SNP
1. **report**：用 `sev =8.0.0`（`default-features=false`，特性 `[snp, crypto_nossl]`，纯 RustCrypto）。一个约 52 行的原型已在真实 GCE report 上通过签名链验证，也能拒绝被篡改的 report。crate 没做的检查要自己补：
   - 版本必须属于 sev 8 支持的集合，未知版本 FAIL；`SIGNATURE_ALGO==1`。
   - VCEK 从 AMD KDS 获取（按 CHIP_ID 和 REPORTED_TCB，带缓存）；验证链 VCEK ← ASK ← ARK（Milan，钉 DER hash）及证书有效期。
   - VCEK 的 hwID == CHIP_ID；VCEK 的 SPL 扩展 == REPORTED_TCB，缺任何扩展都 FAIL。
   - signing_key 为 VCEK，`MASK_CHIP_KEY==0`。
   - DEBUG 和 MIGRATE_MA 都为 0，逐位检查，**不要**求 policy 与背书中的值相等。
   - 四个 TCB 都 ≥ 下限。
   - FAMILY/IMAGE/ID_KEY/AUTHOR_KEY/HOST_DATA 全为 0。
   - VMPL：`snp-hostvtpm` 要求为 0；`snp-svsm` 要求为 0，且 MEASUREMENT 必须等于 `svsm_measurement`。
2. **背书**（从 GCS 获取并缓存，签名链与 TDX 共用）：
   - `snp-hostvtpm`：MEASUREMENT 可以是 `sev_snp.measurements` 中的任意一项（每一项都是 Google 签过的固件），但**不能**是 `svsm_measurement`；
   - `snp-svsm`：MEASUREMENT 只能等于 `svsm_measurement`。
3. **REPORT_DATA**：用 `ledger[0].boot_id` 重新计算。`snp-hostvtpm` 加上 AK；`snp-svsm` 加上 inblob 和 manifest v1。
4. **AK**：
   - `snp-hostvtpm`：验证 Google AK 证书链，终点是钉死的 `EK/AK CA Root`。中间证书走 AIA 获取并验签，不钉死（它在 2026-05 刚轮换过）。要求 isProduction 为真，实例身份与 policy 一致。
   - `snp-svsm`：quote 所用密钥的完整 TPMT_PUBLIC 必须等于 manifest v1 中的某一项，且带 `restricted|sign`。
5. **TPM quote**：先确认 TPMS_ATTEST 的 `magic==0xff544347`、`type==TPM_ST_ATTEST_QUOTE`，PCR selection 恰好是 SHA-256 bank 的 PCR0–9；再验签，确认 `extraData` 覆盖了 nonce、report 和 status（以及可选的 ACPI manifest），且 pcrDigest 与 PCR 值一致。
6. **PCR**：固件日志 replay 等于 PCR0–7 和 PCR9，然后按 4.3 的表逐个 PCR 套规则；按事件类型绑定 digest 的规则与 9.2 第 5 步相同，只是换成 SHA-256 bank。PCR8 等于账本 replay 的结果。
7. `snp-hostvtpm` 模式下，若 policy 没有 `allow_host_vtpm`，**结果为 FAIL**。

---

## 10. 构建与发布
1. 构建容器钉死所有版本：Rust（target 为 `x86_64-unknown-linux-musl`）、musl-gcc（`ring` 需要）、内核工具链、mtools、gdisk。
2. 构建 `/init`：`cargo build --locked --release`，加 `--remap-path-prefix` 和 `codegen-units=1`；`SOURCE_DATE_EPOCH` 取 commit 时间。
3. 构建内核：
   - 使用 sha256 钉死的 6.18.y 源码，打上 BadAML 补丁；
   - 执行 `make allnoconfig KCONFIG_ALLCONFIG=kernel.miniconfig`；
   - 运行断言脚本：miniconfig 中的每个 `=y`，以及 must-be-off 中的每一项，都必须与最终 `.config` 一致；
   - 设置 `KBUILD_BUILD_{TIMESTAMP,USER,HOST,VERSION}`，构建 `bzImage`，即 `BOOTX64.EFI`。
4. 制作磁盘（采用 Flashbots 的做法）：
   - 1 GiB 的 `disk.raw`；
   - `sgdisk` 写入固定的 disk GUID 和 ESP GUID，ESP 从 LBA 2048 开始；
   - `mkfs.vfat --invariant` 格式化，用 `mcopy` 写入 PE；
   - `tar --format=oldgnu -Sczf` 打包。
5. 生成 `ccrun measure disk.raw > release.json`。**可复现的承诺只覆盖 release.json**，也就是 GPT 和 Authenticode；FAT 文件系统的字节能否复现是加分项。CI 在两台机器上独立构建，release.json 必须逐字节一致。
6. `ccrun image publish`：先 `gcloud storage cp`，再 `gcloud compute images create --guest-os-features UEFI_COMPATIBLE,GVNIC,VIRTIO_SCSI_MULTIQUEUE,TDX_CAPABLE,SEV_SNP_CAPABLE[,SNP_SVSM_CAPABLE]`。不带 `SEV_CAPABLE`：普通 SEV 没有证明能力，镜像在那上面启动只会关机。

---

## 11. 使用流程

```bash
ccrun keygen                                    # 生成 ~/.config/ccrun/owner.ed25519
ccrun image publish --project P --bucket B      # 或直接使用官方公共镜像
ccrun up agent1 --tee tdx --zone us-central1-a --machine-type c3-standard-4 \
  --image us-docker.pkg.dev/p/r/agent@sha256:… -p 8080 \
  -e MODEL=x --secret OPENAI_API_KEY=@./key.txt
ccrun verify http://IP:7700 --policy agent1.policy.json
ccrun logs agent1        # 查看串口输出
ccrun down agent1        # 删除实例及其专属防火墙规则
```

`ccrun up` 是**幂等**的，按实例当前状态行事：
- **实例不存在**：先做 preflight（gcloud 认证、项目、zone 是否支持该 TEE、组织策略是否禁止外网 IP），再用**实测过的固定参数**创建：
  - `--confidential-compute-type TDX|SEV_SNP`；SNP 加 `--min-cpu-platform "AMD Milan"`；
  - `--maintenance-policy TERMINATE`；
  - `--network-interface nic-type=GVNIC`；
  - `--boot-disk-type pd-balanced`，只挂这一块盘；拒绝 `-lssd` 机型和额外的盘；
  - `--no-shielded-secure-boot --shielded-vtpm`；
  - `--no-service-account --no-scopes`（`vm-service-account` 模式除外）；
  - `--metadata ccrun-owner=<pub>`；
  - `--tags ccrun-<NAME>`，并为这台实例单独创建一条防火墙规则，只放行 7700 和 `-p` 指定的端口。
- **实例已停止**：启动它。
- **处于 WAITING**：重新验证拉取前状态，然后投递。宿主维护后自动重启的 VM 会回到 WAITING，用这条命令即可恢复。
- **处于 STARTING**（上次的 launch 响应丢了，或者 commit 没发出去）：验证 STARTING 状态，并确认 `ledger[2]` 正是本次请求的 config，然后补发 commit。如果 config 不一致，就报错，并提示 `--recreate`。
- **处于 PULLING**：等待，然后验证。
- **处于 RUNNING**：验证，并确认 config 一致。
- **处于 FAILED 或 EXITED**：这两个是终态，同一次启动不能再 launch。`ccrun up --recreate` 会执行 `gcloud compute instances reset`，得到一次新启动、回到 WAITING，再走正常流程（秘密需要重新投递，CLI 本地都有）。
- 轮询 evidence 的同时也轮询实例状态。实例进入 TERMINATED 或超时，就打印串口末尾（里面有 `ccrun: FAIL <step>`）。
- 遇到基线里没有的 ACPI 或 TD HOB：**直接 FAIL**，并提示运行 `ccrun golden --propose`。**不提供交互式 TOFU。**

`ccrun golden --propose --tee tdx --machine-type c3-standard-4` 的流程：
1. 起一台 VM，让它停在 WAITING；
2. 带 `?acpi=1` 获取 evidence；除 ACPI、TD HOB、CFV 之外的项全部照常验证；
3. 生成提案：digest、`iasl -d` 反汇编结果、与上一版基线的 diff，以及自动检查（是否出现 `Load`/`LoadTable`，是否有指向 RAM 的 `SystemMemory` region）；
4. 删除这台 VM。

提案须经人工审阅并签名后，才能进入 platform.json。

---

## 12. 代码规模与依赖预算（v0.3 重新估计）

| 组件 | 行数 | 依赖 |
|---|---|---|
| init：启动、挂载、状态机、status | 180 | `rustix` |
| init：网络（netlink 配置 lo/eth0、DHCP、resolv.conf） | 200 | `dhcproto` |
| init：HTTP 服务 | 100 | `httparse` |
| init：TEE 后端（configfs-tsm、RTMR3、CCEL；SNP 的 report 和手写 TPM2） | 120 + 180 | 无 |
| init：launch 和 commit（验签、HPKE、CAS、时间下限、一致性锁） | 130 | `ed25519-dalek`、`hpke`、`serde_json` |
| init：registry 客户端（只按 digest 拉取，realm 白名单） | 200 | `ureq 3`（`rustls` + `ring` + `webpki-roots`） |
| init：解包助手（两遍处理、替换语义、多成员 gzip、读到 EOF） | 200 | `tar`、`flate2`、`ruzstd` |
| init：启动助手（ns、卸载挂载、MS_MOVE+chroot、capability、exec 握手、reap） | 240 | `rustix` |
| core：schema、账本、TCG2 解析、按事件类型绑定 | 300 | `sha2`、`serde` |
| cli：gcloud 封装（up 的各个分支、down/logs/golden/preflight/防火墙） | 500 | — |
| cli：collateral 和背书的获取与缓存（PCS、KDS、AIA、GCS） | 180 | `ureq` |
| cli：背书验证（TDX 和 SNP 共用） | 80 | `prost`（derive）、`rsa` |
| cli：TDX verifier（含 TCB 策略） | 340 | `dcap-qvl` |
| cli：SNP verifier（report 补充检查、TPM quote、AK 链、PCR 规则） | 450 | `sev`、`x509-cert`、`rsa` |
| cli：measure | 80 | `authenticode`、`object` |
| 脚本、kernel miniconfig、must-be-off 清单、断言脚本 | 约 300 | — |
| **合计** | **约 3.6k 行 Rust + 约 300 行脚本和配置 + 1 个约 150 行的内核补丁** | |

几点说明：
- verifier 不属于 guest 的 TCB，但**属于 relying party 的 TCB**，同样是安全关键代码。
- 引入 `sev` crate 会让 verifier 同时链接两代 RustCrypto，这是为了减少代码量做的取舍。
- 如果 M0 证明 `snp-svsm` 不可用，而你又决定不发布 `snp-hostvtpm`，就可以砍掉约 650 行代码和 `sev` crate，内核里也不再需要 TPM 驱动。

---

## 13. 剩余风险（诚实声明）
1. **宿主 ACPI**：这是唯一由宿主提供、又在内核里执行的代码。基线审阅的能力有限（iasl 反汇编加人工阅读），而漂移又很频繁：2026 年 c3-standard-4 上至少出现过 5 套三元组，同一次运行里 3 台中就有 1 台不同。运行时的缓解只有 BadAML 补丁，它只拦截访问 RAM 的 SystemMemory region，**不是**完整的 AML 沙箱。而且**审阅看到的表内容（post-patch，在 AML 执行之后读出）与 RTMR0 中的 digest（pre-patch 的 fw_cfg blob）之间没有密码学关联**：审阅者只能相信"同一台 VM 同时给出了这些表和这些 digest"。要真正建立关联，必须能从 post-patch 的表复原 pre-patch 的 blob（v2）。因此 `tdx` 模式下"满足要求 3"的准确表述是：**在信任已审阅的 ACPI digest 的前提下满足**。**这是 TDX 方案最大的残余风险，也是最大的运维负担。**
2. **Google 固件和 SVSM 闭源**：只能依赖 Google 的签名。
3. **设备驱动的攻击面**：gVNIC、tpm_tis、8250、rtc_cmos 都在解析宿主提供的数据，而上游 TDX/SNP guest 既没有设备过滤，也没有为这些驱动做 CoCo 加固。
4. **未度量的 owner 输入**：秘密的明文不在度量范围内。账本里只有密文的 hash，owner 事后可以把明文打开给审计者核对。
5. **公开信息**：config 和 env 是公开的；串口日志（`logs=serial` 时）宿主可见。
6. **凭证机密性**：依赖 WebPKI 加 `issued_at` 的时间下限，并且不检查证书吊销。
7. **可用性**：宿主维护后需要 owner 再执行一次 `ccrun up`；ACPI、固件或 TCB 的变化可能造成误报 FAIL，需要更新基线。
8. **`snp-hostvtpm`**：在恶意宿主下不满足要求 3。
9. **workload 自身**：ccrun 只证明"跑的是它"。workload 一旦被攻破，可以泄露它自己持有的秘密。

---

## 14. M0：动手前必须上真机确认的事项（约 2 天）
1. **TDX（c3-standard-4）**：
   - 普通 EFI stub PE（allnoconfig 内核）作为 `BOOTX64.EFI` 能启动；
   - RTMR1 正好是 6 个事件（EBS 重试的那条轨迹只能从源码推导，真机上很难触发，release.json 两条都给出），RTMR2 为 0；
   - 使用 `--no-shielded-secure-boot` 时，RTMR0 的事件语法与 §4.2 一致；
   - 写 RTMR3 后 replay 结果一致。
2. **对抗启动测试**：逐项注入，确认要么 PE 不受影响，要么 verifier FAIL：
   - NVRAM 中设置 `BootNext` 加 `initrd=\x`；
   - LoadFile2 方式的 initrd；
   - `Driver####`；
   - 畸形的 LoadOptions；
   - 篡改 Boot 变量。
3. **网络**：
   - GCE 的 DHCP 是否单播回复（决定是否需要 BROADCAST 标志）；
   - option 121、lo、resolv.conf 是否正常；
   - metadata 和外网 registry 是否可达；
   - 租约到期后 IP 是否被收回。
4. **镜像**：同时声明 `TDX_CAPABLE` 和 `SEV_SNP_CAPABLE` 的同一个镜像，能否在 C3 和 N2D 上都启动；N2D 上是否用 gVNIC。
5. **N2D SNP**：
   - `KVM_GUEST=n` 时的时钟源；
   - PCR2/3/6 是否只有 SEP；PCR5 的精确事件语法；
   - PCR4 是否只有一个应用事件；
   - vTPM 是否走 TIS；
   - auxblob 里有什么。
6. **`snp-svsm`**（决定 SNP 能否发布）：
   - 普通项目能否使用 `--svsm-config`；
   - `tpm_svsm` 驱动是否可用，guest 是否在 VMPL2；
   - **能否拿到 manifest v1（含 AK）**；AK 模板放在哪个 NV index；
   - OVMF 是否把 `BOOTX64.EFI` 度量进 SVSM vTPM 的 PCR4；
   - PCR1 中是否有 ACPI；
   - MEASUREMENT 是否等于 `svsm_measurement`。
7. **`--no-shielded-vtpm` 能否用于 TDX**：若能，可以彻底去掉 TDX 上的 vTPM 设备，但 RTMR2 会因此多出 LoadOptions，需要重新评估。

---

## 15. 后续与可选项
- **v1.1 metadata 自启动**：面向不需要秘密的场景。metadata 中放一份 owner 签名的 `ccrun-config`，`/init` 先度量再自动拉取，重启后自动恢复。约 40 行。
- **v2**：
  - 从背书的 `.fd` 离线推导 CFV 和 TD HOB；
  - 尝试从打补丁后的 ACPI 表复原打补丁前的 fw_cfg blob，使 ACPI digest 能由内容重算；
  - 加密的 scratch disk，支持超大镜像；
  - A3 H100 CC。
