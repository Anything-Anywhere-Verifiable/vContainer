# v0.1 审阅处置记录

审阅对象是 [DESIGN-v0.1.md](DESIGN-v0.1.md)，处置结果都已体现在 [DESIGN.md](DESIGN.md)（v0.2）中。

审阅来源有两个：
- **Codex**：原文见 [REVIEW-codex.md](REVIEW-codex.md)，下文用 C1–C14 编号。
- **Claude**：从 4 个视角审阅（TDX G3 完整性、SNP、协议与隔离、最小化/易用/可行性），每个视角配一个反驳者逐条复核，共 39 条。下面的严重度是**复核后**的结论。

处置说明：
- **接受**：按发现的建议修复。
- **部分接受**：采纳其中成立的部分。
- **驳回**：理由写在表中。

## 高 / 严重

| 发现 | 来源 | 处置 | v0.2 位置 |
|---|---|---|---|
| workload 改写 `/proc/sys/kernel/core_pattern`，内核会以初始命名空间的 root 身份执行它指定的 helper | C1（Critical）；Claude 反驳者也独立复现 | **接受**：`STATIC_USERMODEHELPER` 并设空路径（禁用全部 usermode helper），`COREDUMP=n`，`/proc/sys` 只读，同时遮蔽 sysrq-trigger、kcore 等 | §4.1、§6.5 |
| 宿主可通过串口 BREAK 或 PS/2 触发 SysRq，dump 出 GPR | Claude tdx-g3（high，确认） | **接受**：`MAGIC_SYSRQ=n`、`VT=n`、`INPUT_KEYBOARD=n`；workload 的 stdin 接 `/dev/null`；PID1 的 fd 0–2 指向 `/dev/null` | §4.1、§4.4、§6.2、§6.5 |
| 拥有 `CAP_SYS_CHROOT` 的 workload 可以连续 chroot 逃出，碰到继承下来的 configfs、可写 sysfs 和 TEE 设备 | Claude protocol（复核降为 medium） | **接受**：在子 mount namespace 里 `umount2(MNT_DETACH)` 卸掉 init 的全部挂载，改用 `MS_MOVE`+chroot，并去掉 `SYS_CHROOT` | §6.5 |
| SNP 的 PCR1（宿主 ACPI）和 PCR2/3/6（Driver####、option ROM）没有任何规则 | C2；Claude snp ×2（high，确认） | **接受**：PCR1 改用与 TDX 相同的 ACPI 基线；PCR2/3/6 精确 pin 为只含 SEP 的常量；删掉"SNP 不需要 golden"的说法 | §4.3、§9.3 |
| golden 和 TOFU 只是在盲收宿主 ACPI | C3；Claude（复核降为 medium） | **接受**：取消交互式 TOFU；golden 只生成提案（包含 iasl dump、diff 和自动检查），须人工审阅签名后才生效；BadAML 补丁强制；文档措辞从"审计"改为"pin" | D11、§9.1、§11、§13 |
| SVSM 若不指定 manifest 版本，默认 v0 只含 EK，无法绑定 AK | C4；Claude snp（medium，确认） | **接受**：`service_manifest_version=1`，用完整 TPMT_PUBLIC 比对并要求属性正确；拿不到 v1 就放弃 SVSM 模式 | §4.3、§9.3、§14 |
| 按事件标签作判断（PCR9、SecureBoot 等），而标签不在 digest 覆盖范围内 | C5；Claude snp（low） | **接受**：按事件类型绑定 digest（H(data)、H(VariableData) 或不透明比较）；PCR9 不再评估，`noinitrd` 由构造保证；明确要求 MRTD 必须出现在背书的签名内容里 | §9.2 第 5 步、§4.3 |
| evidence 里的 `state` 没有被签名覆盖，宿主可把 exited 改成 running | C6 | **接受**：status 绑定进 REPORTDATA 或 qualifyingData；增加 exec 握手；FAILED 为终态 | §6.1、§6.5、§7.1 |
| 私有仓库凭证的机密性依赖 TLS PKI 和时间 | C7；Claude（low） | **接受**：只用短期 token；token 只发往指定的 registry 主机和 realm 白名单；用 owner 签名的 `issued_at` 作为 RTC 下限 | §6.3、§13 |
| `snp-hostvtpm` 的信任表述不准确：TIS MMIO 由 VMM 模拟，所以要信任的是整个宿主，而不只是 vTPM | Claude snp（复核降为 medium） | **接受**：措辞改为"信任整个 Google 宿主"，结果标为 `root=amd+google-host`；policy 不写 `allow_host_vtpm` 就 FAIL | §0、§1.3、§4.3 |
| policy 没有固定 TEE 模式，存在降级风险 | Claude snp（high，部分成立） | **接受**：`policy.tee` 改为必填；不同模式使用互斥的 measurement 选择规则 | §9.1、§9.3 |

## 中

| 发现 | 来源 | 处置 | v0.2 位置 |
|---|---|---|---|
| 从 defconfig 起步，带进了大量解析宿主设备的驱动；而 `=n` 会被 `select` 改回去（例如 ATA→SCSI） | Claude ×2 | **接受**：改为 allnoconfig、正向白名单、`merge_config.sh -s`，外加 must-be-off 断言 | D10、§4.1 |
| `VIRT_DRIVERS` 漏写，TDX/SNP 驱动会被静默丢弃 | Claude（确认） | **接受**：加入白名单，并由严格合并兜底 | §4.1 |
| 多线程 PID1 中 `unshare(CLONE_NEWPID)` 有问题 | C9 | **接受**：改由单线程启动助手执行 `unshare` 再 fork | §6.5 |
| PID1 没有 resolv.conf；`lo` 没有启用 | C12；Claude ×2 | **接受** | §4.1、§6.2、§6.4 |
| OCI 替换语义（目录与文件互换、硬链接重发）和 opaque whiteout 的顺序问题 | C11；Claude ×2 | **接受**：每层两遍处理，先 lstat 再删除 | §6.4 |
| gzip 只解第一个成员、zstd 只解单帧，导致内容被静默截断，与 Docker 看到的不一致 | Claude（确认） | **接受**：改用 `MultiGzDecoder`；读到 EOF 并要求 blob 被完整消费 | §6.4、§8 |
| launch 缺少原子转换 | C8；Claude（low） | **接受**：先做无副作用的检查，再 CAS；之后失败一律进入终态 | §6.3 |
| `up` 不幂等，宿主维护之后无法恢复；创建参数与实测过的组合不一致；挂了默认服务账号 | C13；Claude ×2 | **接受**：`up` 改为幂等，固定实测参数，默认 `--no-service-account --no-scopes`，每个实例单独一条防火墙规则 | §11 |
| golden 以 MRTD 为键，每次固件轮换都会失效；第三方手上没有 platform.json | Claude（部分成立） | **接受**：ACPI 改按机型为键；`up` 把相关基线拷进自包含的 policy | §9.1 |
| SNP 规范前后矛盾（PCR8 被放进固件日志 replay、REPORT_DATA 写法不一致） | C10；Claude（low） | **接受**：PCR8 单独 replay；boot_id 统一从 `ledger[0]` 取 | §4.3、§9.3 |
| MROWNER 实例绑定是可选项，第三方可以用你公开的 owner key 劫持 launch | Claude（low） | **接受**：`up` 强制检查，并写入 policy | §4.2 |
| 只认 `UpToDate`：Intel 发布 TCB recovery 后整个机群都会 FAIL | Claude（low） | **接受**：按固定的 eval number 取 collateral，更新的状态只作警告 | §9.2 |
| tmpfs 内存峰值过高；下载大小没有上限 | Claude（low） | **接受**：逐层流水线处理，读到 `descriptor.size+1` 即中止 | §6.3 |

## 低、驳回或延后

| 发现 | 处置 |
|---|---|
| G4 写"零 C 依赖"不实（ring、musl） | 接受，已改写 G4 |
| config 和 env 实际上是公开的，§0 和 §13 没有说清 | 接受，已在 §0、§7.2、§13 写明 |
| RTMR1 存在合法的 EBS 失败变体 | 接受，RTMR1 改为语法检查，允许一个 `EBS Failed` 事件 |
| DHCP 若单播回复会收不到 | 接受，设置 BROADCAST 标志；写入 M0 |
| SMBIOS、CPUID 0x4000_00xx 等输入没有列清 | 接受：`DMI=n`、`nopv`；新增输入清单 §4.4 |
| 秘密密文没有进账本 | 部分接受：账本 #2 加上 `SHA-512(enc‖ct)`，供事后审计 |
| TD HOB 和 CFV 可以离线推导，不必走 golden | 延后到 v2（大约 150 行）；v1 用审阅过的基线 |
| 应用连接没有认证（宿主可以在 :7700 转发真实 evidence，同时替换 :8080） | 接受，用最小方案：config 增加 `tls_spki_sha256`，私钥经 `--secret` 投递 |
| 继承 fd 导致 TPM 和 sysfs 泄露 | **驳回**（复核者已驳回）：Rust std 默认 O_CLOEXEC。作为保险仍加了一行 `close_range` |
| capability 应改为空白名单 | **驳回**（复核者已驳回）：会破坏 gosu、chown 一类常见镜像，与 G1 冲突；只去掉 `MKNOD` 和 `SYS_CHROOT` |
| 内核 LPE 攻击面（io_uring、userfaultfd、BPF） | 部分接受：allnoconfig 加断言即可覆盖，不需要额外代码 |
| 改用 metadata 自启动作为默认模式（Claude cut） | **驳回作默认**：agent 需要秘密，而 G3 的核心是"先验证后投递"。列为 v1.1 可选项（§15） |
| 彻底删掉 SNP-vTPM（Claude cut） | **部分接受**：你明确要一个 SNP 方案，所以保留，但改名为 `snp-hostvtpm`，必须显式授权；是否发布 SNP 由 M0 第 6 项决定 |
| 删除 MROWNER 和实例绑定（Claude cut） | 驳回：成本约 10 行，能防非 Google 的劫持 |
| 删除 init 自查寄存器初值（Claude cut） | 接受：verifier 从零 replay 已经能 fail closed |
| UTS/IPC namespace 用处不大（C cut） | 接受，只保留 NEWNS 和 NEWPID |
| SNP 的 VCEK 和背书只用 KDS/GCS 加缓存，不用 auxblob（Claude cut） | 接受（auxblob 的内容尚未实测） |
| 去掉 `SEV_CAPABLE`（Claude cut） | 接受 |

## 仍需你决定的问题
1. **是否发布 SNP**：取决于 M0 第 6 项（SVSM 的 manifest v1 能否拿到）。若 SVSM 不可用，是"只发布 TDX"，还是"同时发布带警告的 `snp-hostvtpm`"？
2. **ACPI 基线由谁审阅、多久更新一次**：这是 TDX 方案主要的运维成本。可以参考 Tinfoil 的 hardware-measurements 仓库，由维护者签名发布。
3. **内核版本**：6.18 LTS 加 MS_MOVE+chroot，还是直接上 7.x（有 nullfs，可用 pivot_root，但不是 LTS）？

---

# 第二轮：Codex 审阅 v0.2（[REVIEW-codex-r2.md](REVIEW-codex-r2.md)）→ v0.3

Codex 对第一轮 14 条的复核结果：C1、C2、C4、C9、C11、C14 已修复；C3、C5、C6、C7、C8、C10、C12、C13 部分修复。本轮新发现及处置如下：

| # | 发现 | 处置（v0.3） |
|---|---|---|
| N1 | `merge_config.sh -s` 遇到"请求值 ≠ 生效值"只告警，不会失败；allnoconfig 还缺 `SHMEM`、`MULTIUSER`、`TCG_SVSM` 等基础选项 | 改用 `allnoconfig KCONFIG_ALLCONFIG=miniconfig`，另加独立断言脚本；补齐基础选项；最终以 M0 能启动为准（§4.1、§10） |
| N2 | TCB 策略在 v0.2 中丢失；按旧 eval 版本取 collateral，最终会遇到 HTTP 410 | 状态和 advisory 由我们显式检查，加上 `nextUpdate`；eval 版本取下限；`tcb_grace_until` 带截止日期（§9.2 第 1 步） |
| N3 | `close_range` 会把握手管道一起关掉；子进程退出也会读到 EOF | 改用 `CLOSE_RANGE_CLOEXEC`；失败路径写 `(step, errno)`；`running` 的语义收窄为"execve 已提交，并且取 quote 时进程仍存活"，用 `waitpid(WNOHANG)` 确认（§6.5） |
| N4 | 状态与账本前缀互相矛盾；快照不一致 | 给出状态与账本前缀对照表；extend、状态变化、生成 evidence 共用一把锁；只有账本中存在 #2 时才比对 config（§5、§9.2） |
| N5 | 事件白名单会拒绝真实证据：index 0 的 `NO_ACTION`、SNP 的 S-CRTM/NonHostInfo、PCR5 的 SEP | `NO_ACTION` 单独处理；补充 SNP 的事件类型；PCR5 的语法以 M0 实测为准（§4.3、§9.2 第 4–5 步） |
| N6 | `issued_at − 299s` 仍然能通过；origin 没有包含 scheme 和 port | 用 `clock_settime(max(RTC, issued_at))` 设定时间下限；授权单位改为完整的 HTTPS origin（§6.3） |
| N7 | SVSM 模式同样需要读 AK 模板；SVSM 下没有实例身份 | 两种模式都读模板（NV index 由 M0 确认）；`snp-svsm` 的实例绑定为空（§4.3、§9.1） |
| N8 | `up` 在 STARTING、PULLING、FAILED、EXITED 状态下的行为没有定义 | 每个状态都写明了分支；增加 `--recreate`，通过 reset 获得一次新的启动（§11） |
| N9 | "自包含"的 policy 里缺少 release 身份 | 把 PE、GPT 的 digest、RTMR1 两条轨迹、各 PCR 期望值拷入 policy（§9.1） |
| N10 | `acpi=1` 时缺少精确的绑定公式 | 定义 canonical `acpi_manifest` 及其在 TDX/SNP 下的绑定方式，并写明它与 RTMR0 之间没有密码学关联（§7.1） |
| N11 | RTMR1 重试轨迹与 M0 的"恰好 6 个事件"互相矛盾 | release.json 同时给出两条轨迹（§4.2、§14） |
| 阻断项 | G3 的时序：远端在拉取之前能否看到 config 的度量 | **接受**：新增 prepare → attest → commit 两阶段（STARTING 状态 + `POST /v1/commit`），约 25 行，CLI 自动完成（§0、§6.3、§7.3） |
| 阻断项 | ACPI 审阅内容与 RTMR0 digest 之间没有密码学关联 | **接受为已声明的残余风险**：TDX 的结论改为"在信任已审阅 ACPI digest 的前提下满足 G3"；真正建立关联要靠复原 pre-patch blob，列入 v2（§13.1、§15） |
| 阻断项 | M0 和对抗启动测试尚未完成 | 仍是开工前提（§14） |
