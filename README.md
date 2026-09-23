# elan-haptune

A small, one-shot Rust CLI for ELAN haptic touchpad thresholds. Linux x86_64 is
software-tested; the hidraw encoding also targets Linux aarch64 (untested).

The only enabled profile is the Redrix HP Elite Dragonfly Chromebook's I2C
**ELAN2703, 04f3:323b**, with exact register identity `0x0130/0x1300`. Prior user
experiments support threshold adjustment on this device. This Rust implementation
has **not been hardware-write tested**. ThinkPad Z13/Z16 Gen 1 provide vendor
protocol evidence, not tested support. Other ELAN devices are rejected.

## Build and use

Built and software-tested with Rust/Cargo 1.95 and a C linker. From this repository:

```sh
cargo build --release --locked
./target/release/elan-haptune list
./target/release/elan-haptune get --json
./target/release/elan-haptune set --dry-run \
  --press-threshold 120 --release-threshold 95 --drag-release-threshold 60
```

After reviewing the dry-run, removing `--dry-run` applies the requested settings.
The example explicitly chooses drag release 60 (a vendor preset); it does not
reconstruct the lost historical 120/95 experiment's drag value. To preserve drag
release, omit that option; if its current value is at least the requested press
threshold, validation will reject the update.

`--haptics on|off` controls the vendor's feedback-enable bit while preserving
every other control bit. Its mapping is statically verified, but its behavior on
this target still needs a hardware test. `--haptic-level` is deliberately absent:
the vendor's encodings are known, but physical intensity ordering and safe target
interpretation are not. There are no TrackPoint controls.

By default, `get`/`set` require exactly one compatible physical device. Use
`--device /dev/hidrawN` or `--device 'sysfs:/devices/…'` with the selector printed
by `list`. The sysfs selector omits the transient HID instance number and survives
hidraw renumbering/rebind on unchanged topology. Firmware/kernel topology changes
can change it. Explicit paths, including symlinks to hidraw nodes, undergo the same
model, opened-descriptor, and protocol identity checks.

`list` reads sysfs only. `identity_check_required` means the model is recognized,
not that its protocol identity has been queried. Unsupported devices are listed
without opening them. `get` and `set --dry-run` send query feature reports, whose
request uses **SET_FEATURE**, but issue **no parameter mutations**. These operations
require read/write access to the selected hidraw node. Permission errors are
reported; the CLI never elevates privileges or changes permissions.

## Validation and failure behavior

Thresholds are device raw units, not force measurements or percentages. Firmware
limits are unknown (`firmware_ranges: null`). The CLI intentionally restricts
explicit threshold requests to these **local policy bounds**, derived from the
envelope of retained vendor presets, probe values, and user reports:

| Setting | CLI bounds (inclusive) |
| --- | --- |
| Press | 120–192 |
| Regular release | 95–154 |
| Drag release | 60–125 |

These bounds do not establish firmware legality or prove that every intermediate
value/combination is safe. Both release thresholds must be positive and strictly
below press. Unspecified fields stay unchanged, including values outside the CLI
bounds if the complete threshold state satisfies hysteresis. No fallback values
or guessed defaults are used. Regular release and drag release are independent.

The CLI holds an exclusive, nonblocking advisory `flock` on the physical sysfs
directory from before the first query through verification/recovery. Concurrent
invocations fail with `busy` instead of interleaving query or write reports. The
lock covers this program's processes; the kernel, vendor tools, and other writers
do not necessarily honor it. Do not run another settings tool concurrently.

All requested values and combined constraints are validated before mutation.
Unchanged values are skipped. Increasing press precedes release changes; lowering
press follows them. Feedback is disabled first or enabled last when requested.
Each attempted write is read back; the full state is also checked between writes
and at completion. There is no atomic multi-register transaction.

On failure, the CLI reads the actual state and attempts a verified, safely ordered
restoration of attempted settings. It stops if state is unreadable, hysteresis is
invalid, unrelated settings/bits changed, or recovery fails. It reports attempted
and verified changes, observed state (or null), and rollback status
`restored`, `incomplete`, or `unknown`. A failed operation exits 1 even if rollback
succeeds. Process termination, power loss, or disconnect can leave a partial update;
there is no crash recovery or persistence promise across suspend/reboot.

## Output and development

`--json` emits one object on stdout:

```json
{"schema_version":1,"command":"list","ok":true,"data":{"devices":[]},"error":null}
```

`get` data contains `device`, `settings`, and `capabilities`; `set` contains `device`
and `update`. Errors have `kind`, `message`, and `exit_code`; preflight errors have
null data, while partial update errors retain the update report. Parsing errors
use command `parse`. Diagnostics go to stderr. Help/version remain plain text.

| Exit | Meaning |
| --- | --- |
| 0 | Success, dry-run, or no-op (including an empty list) |
| 1 | Permission, busy, I/O, protocol, verification, or update failure |
| 2 | Invalid command or requested settings |
| 3 | No matching compatible device |
| 4 | Ambiguous physical device/interface selection |
| 5 | Explicitly selected unsupported device/identity, or unsupported current state |

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

On NixOS, missing development tools can be provided temporarily with
`nix shell nixpkgs#gcc nixpkgs#rustfmt nixpkgs#clippy`; no flake or installation is
required. Tests use a stateful report-level simulator and CLI subprocesses; none
issue live HID feature reports. See [protocol evidence](docs/evidence.md) for
provenance, design decisions, and remaining hardware validation.
