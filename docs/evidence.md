# Evidence and implementation boundary

Analysis on 2026-09-22 used retained local artifacts under
`/home/codgi/touchpad-session-recovery-20260922`. Proprietary executables were
inspected with `strings` and `objdump -d -C`; none were executed or copied into
this repository. Symbol names/addresses below identify the evidence, not recovered
source code or an open-source licensing claim.

## Vendor evidence

Lenovo package `N3GG601W` describes ThinkPad Z13 Gen 1 and Z16 Gen 1 support.
Its README says version 1.3, while its version table says 1.0, build N3GG601W,
2023/03/03. That discrepancy is retained.

SHA-256 of retained binaries in `vendor-package/source/ehps-install/`:

```text
ElanHapticPadSettings
56e237d5499feb8b5e4d1df982765dcad9f734dda2a3c8eeb61418c6fa86d368
ehpsettings
a9089454e191d7cfb59fa7a18ecef8a0f82a01e1ee95333a609618db27dfee36
```

GUI binary analysis reconfirmed:

| Routine/address | Finding |
| --- | --- |
| `hid_read_cmd` 0xa720; `hid_send_cmd` 0xa5d0 | Query `[0x0d,0x05,0x03,reg_lo,reg_hi]`, followed by GET_FEATURE; register echo and little-endian value |
| `hid_write_cmd` 0xa7c0; `elan_write_cmd` 0xa8a0 | Parameter write `[0x0d,reg_lo,reg_hi,value_lo,value_hi]` |
| `WriteCommandVerify` 0xabb0 | Write, 500 µs delay, readback comparison |
| `scanning` 0xa8f0 | Reads 0x0101 and 0x0103; compares first value and **high byte** of second to caller's identifiers |
| `Set_hapticclick_force` 0xb800 | Press/release/drag presets 122/98/60, 160/128/60, 192/154/60 |
| `Load_hapticclick_force_push/release/releasedrag` 0xb700/0xb760/0xb7c0 | Separate registers 0x03a2, 0x03a3, 0x03a4 |
| `Set_hapticfeedback` 0xb530; `Load_hapticfeedback` 0xb5a0 | Read-modify-write/read bit **8**, mask 0x0100, of 0x03a1 |
| `Set_usetopzone_pstbtn` 0xb930 | Separate bit **1**, mask 0x0002, of the same register |
| `Set_hapticfeedback_state` 0xb5d0; loader 0xb690 | Indices 0..4 map to 0x0000, 0x5050, 0x4040, 0x3030, 0x2020 at 0x03ab |
| `Set_pstbtn_force` 0xba60 | Registers 0x03a9/0x03aa, presets 80/60, 110/66, 140/84; untouched by this CLI |

The loader binary's `main` at 0x12a0 reads six bytes from
`/var/cache/etphps.config`, applies settings, and exits. It first requests identity
0x000e/0x13, then 0x000d/0x13, using a broad ELAN PID scan. The new CLI does **not**
reuse those broad matching rules. Identity registers are treated as opaque
compatibility guards, not asserted firmware versions or a capability bitmap.
The startup scripts run the loader from init.d/rc2.d. It is not a daemon or a
parameterized command-line utility.

The haptic-level byte meanings, intensity order, meaning of index zero, and behavior
on ELAN2703 remain unverified. Consequently no level setter or decoded level getter
is exposed. Haptics on/off has a precise boolean mapping, separately from level;
the CLI changes only bit 8 and verifies the complete register.

## Target-specific evidence and policy

The retained C probes identify 04f3:323b and exact register values
0x0101=0x0130 and 0x0103=0x1300. They use five-byte reports with report/register
echo validation and a 1 ms post-write delay. The new code keeps this stricter
full-value identity guard, and additionally requires I2C and ELAN2703 sysfs identity.
This is one allowlisted profile, not broad ELAN/ThinkPad support or a claim that
every firmware with this identity has been tested.

`elan-threshold-test.c` contains original values 150/125/125 and trial
120/100/100. Its old coupling of the two release registers is not retained.
Actual recovered user prompts report successful 120/100 and a preference for
120/95, but the final triple/command transcript is missing. We do not claim its
drag value, firmware range, or persistence is known.

No range-query protocol was established. The wire transports 16-bit integers,
which is not evidence that all 16-bit values are sensible thresholds. The CLI
therefore chooses a conservative local request envelope: press 120..192, regular
release 95..154, drag release 60..125. These are a software policy assembled from
the retained values, **not measured hardware limits**, and include untested
interpolations. The positive, release-below-press constraint is likewise an
explicit hysteresis policy supported by the observed presets/experiments, not a
claimed firmware specification. Unknown actual readings are never substituted
with a preset. Widening the policy requires new evidence and a code change.

## Design and verification

One crate: `main.rs` handles CLI/output, `device.rs` handles sysfs selection,
opened-descriptor validation and locked hidraw access, `protocol.rs` handles report
framing/identity, and `elan.rs` handles settings, pure validation/planning, and
verified recovery. Dependencies are clap (parsing/help), serde/serde_json (JSON),
and libc (hidraw/flock ABI). No daemon, config loader, deployment, or generic
register interface is included.

Software tests exercise the actual codec and update engine against a stateful
report-level simulator, with independent read failures, write failures before and
after mutation, malformed replies, mismatches, concurrent changes, and failed
rollback. Every simulated intermediate threshold state is checked for hysteresis.
CLI subprocess tests exercise help, validation, JSON errors, exit codes, and
sysfs-only discovery. Locking was also checked against the real sysfs physical
directory without accessing hidraw. This does not establish hardware behavior.

Remaining hardware validation requires a functioning target with authorized
read/write hidraw access: first `get` and `set --dry-run` to verify query behavior,
then **separate user authorization** for live updates and restoration. Check each
release independently and haptics with unrelated control bits preserved. Do not
infer persistence without separately authorized suspend/reboot testing. This
implementation session performs no live feature queries or parameter writes,
driver rebinds, suspend/reboots, deployment, commits, or pushes.
