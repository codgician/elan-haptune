use crate::{
    elan::{self, REGS, State, Update},
    error::{Error, Result},
    protocol::{FeatureIo, Protocol},
};
use std::collections::BTreeMap;

// A stateful, strict report-level device simulator. Values survive requests;
// failures can happen before or after mutation, and reads are independently gated.
pub(super) struct Pad {
    pub(super) registers: BTreeMap<u16, u16>,
    pending: Option<u16>,
    writes: Vec<(u16, u16)>,
    fail_writes: Vec<usize>,
    fail_after_apply: bool,
    corrupt_write: Option<usize>,
    fail_reads_after_write: bool,
    short_query: bool,
    read_count: usize,
    external_change: Option<(usize, u16, u16)>,
}

impl Pad {
    pub(super) fn new(state: [u16; 4]) -> Self {
        let mut registers: BTreeMap<_, _> = REGS.into_iter().zip(state).collect();
        registers.extend([
            (0x0101, 0x0130),
            (0x0103, 0x1300),
            (0x03ab, 0x4040),
            (0x03a9, 80),
            (0x03aa, 60),
        ]);
        Self {
            registers,
            pending: None,
            writes: Vec::new(),
            fail_writes: Vec::new(),
            fail_after_apply: false,
            corrupt_write: None,
            fail_reads_after_write: false,
            short_query: false,
            read_count: 0,
            external_change: None,
        }
    }
    fn state(&self) -> State {
        State(REGS.map(|r| self.registers[&r]))
    }
}

impl FeatureIo for Pad {
    fn settle(&self) {}
    fn set_feature(&mut self, report: &mut [u8; 5]) -> Result<usize> {
        assert_eq!(report[0], 13);
        if report[1..3] == [5, 3] {
            let reg = u16::from_le_bytes([report[3], report[4]]);
            assert!(
                self.registers.contains_key(&reg),
                "unexpected query {reg:04x}"
            );
            self.pending = Some(reg);
            return Ok(if self.short_query { 4 } else { 5 });
        }
        let reg = u16::from_le_bytes([report[1], report[2]]);
        let value = u16::from_le_bytes([report[3], report[4]]);
        assert!(REGS.contains(&reg), "unexpected mutation {reg:04x}");
        self.writes.push((reg, value));
        let fail = self.fail_writes.contains(&self.writes.len());
        if !fail || self.fail_after_apply {
            self.registers.insert(
                reg,
                if self.corrupt_write == Some(self.writes.len()) {
                    value + 1
                } else {
                    value
                },
            );
            // Check intermediate hysteresis, not just the final state.
            let s = self.state();
            assert!(
                s.0[1] < s.0[0] && s.0[2] < s.0[0],
                "unsafe intermediate state: {s:?}"
            );
        }
        if fail {
            return Err(Error::protocol("injected write failure"));
        }
        Ok(5)
    }
    fn get_feature(&mut self, report: &mut [u8; 5]) -> Result<usize> {
        self.read_count += 1;
        if let Some((at, reg, value)) = self.external_change
            && at == self.read_count
        {
            self.registers.insert(reg, value);
        }
        assert_eq!(*report, [13, 0, 0, 0, 0]);
        let reg = self.pending.take().expect("GET without query selection");
        if self.fail_reads_after_write && !self.writes.is_empty() {
            return Err(Error::protocol("injected disconnected device"));
        }
        let [rl, rh] = reg.to_le_bytes();
        let [vl, vh] = self.registers[&reg].to_le_bytes();
        *report = [13, rl, rh, vl, vh];
        Ok(5)
    }
}

#[test]
fn external_changes_stop_updates_and_are_not_blindly_rolled_back() {
    for at in [5, 10] {
        let mut io = Protocol(Pad::new([150, 125, 60, 0x100]));
        io.0.external_change = Some((at, 0x03a1, 0x102));
        let report =
            elan::apply(&mut io, &threshold_update(Some(120), Some(95), None), false).unwrap();
        assert_eq!(report.error.unwrap().kind, "concurrent_change");
        assert_eq!(io.0.registers[&0x03a1], 0x102);
        if at == 5 {
            assert!(report.rollback.is_none());
            assert!(io.0.writes.is_empty());
        } else {
            assert_eq!(report.rollback.unwrap().status, "incomplete");
            assert_eq!(io.0.writes, [(0x03a3, 95)]);
        }
    }
}

#[test]
fn haptics_rollback_does_not_overwrite_trackpoint_bit_changed_externally() {
    let mut io = Protocol(Pad::new([150, 125, 60, 0x100]));
    io.0.external_change = Some((9, 0x03a1, 0x0002));
    let report = elan::apply(
        &mut io,
        &Update {
            thresholds: [None; 3],
            haptics: Some(false),
        },
        false,
    )
    .unwrap();
    assert_eq!(report.status, "failed");
    assert_eq!(report.rollback.unwrap().status, "incomplete");
    assert_eq!(io.0.registers[&0x03a1], 2);
    assert_eq!(io.0.writes.len(), 1);
}

fn threshold_update(p: Option<u16>, r: Option<u16>, d: Option<u16>) -> Update {
    Update {
        thresholds: [p, r, d],
        haptics: None,
    }
}

#[test]
fn identity_checks_both_exact_values_without_mutations() {
    let mut io = Protocol(Pad::new([150, 125, 125, 0x101]));
    io.check_identity().unwrap();
    for (reg, bad) in [(0x0101, 0x000e), (0x0103, 0x1301), (0x0103, 0x1400)] {
        let original = io.0.registers.insert(reg, bad).unwrap();
        assert_eq!(io.check_identity().unwrap_err().exit_code, 5);
        io.0.registers.insert(reg, original);
    }
    io.0.short_query = true;
    assert_eq!(io.check_identity().unwrap_err().kind, "protocol");
    assert!(io.0.writes.is_empty());
}

#[test]
fn dry_run_plans_queries_only_and_preserves_independent_drag() {
    let mut io = Protocol(Pad::new([150, 125, 60, 0xa303]));
    let before = io.0.registers.clone();
    let report = elan::apply(&mut io, &threshold_update(Some(120), Some(95), None), true).unwrap();
    assert_eq!(report.status, "dry_run");
    assert_eq!(
        report.planned.iter().map(|c| c.setting).collect::<Vec<_>>(),
        ["release_threshold", "press_threshold"]
    );
    assert_eq!(report.requested["drag_release_threshold"], 60);
    assert_eq!(io.0.registers, before);
    assert!(io.0.writes.is_empty());
}

#[test]
fn complete_update_validated_before_any_write_including_unchanged_drag() {
    let mut io = Protocol(Pad::new([150, 125, 125, 0x101]));
    assert!(elan::apply(&mut io, &threshold_update(Some(120), Some(95), None), false).is_err());
    assert!(io.0.writes.is_empty());
    assert!(elan::apply(&mut io, &Update::default(), false).is_err());
    for update in [
        threshold_update(Some(119), None, None),
        threshold_update(None, Some(155), None),
        threshold_update(None, None, Some(0)),
        threshold_update(Some(65535), None, None),
    ] {
        assert!(elan::apply(&mut io, &update, false).is_err());
    }
    assert!(io.0.writes.is_empty());
}

#[test]
fn lower_releases_before_press_and_raise_press_before_releases() {
    let mut io = Protocol(Pad::new([150, 125, 125, 0x101]));
    let update = threshold_update(Some(120), Some(95), Some(100));
    assert_eq!(
        elan::apply(&mut io, &update, false).unwrap().status,
        "applied"
    );
    assert_eq!(io.0.writes, [(0x03a3, 95), (0x03a4, 100), (0x03a2, 120)]);
    io.0.writes.clear();
    let update = threshold_update(Some(192), Some(154), Some(125));
    assert_eq!(
        elan::apply(&mut io, &update, false).unwrap().status,
        "applied"
    );
    assert_eq!(io.0.writes, [(0x03a2, 192), (0x03a3, 154), (0x03a4, 125)]);
}

#[test]
fn all_policy_endpoint_transitions_preserve_hysteresis() {
    for p in [120, 150, 192] {
        for r in [95, 125, 154] {
            for d in [60, 100, 125] {
                if r >= p || d >= p {
                    continue;
                }
                for np in [120, 150, 192] {
                    for nr in [95, 125, 154] {
                        for nd in [60, 100, 125] {
                            if nr >= np || nd >= np {
                                continue;
                            }
                            let mut io = Protocol(Pad::new([p, r, d, 0xa303]));
                            let result = elan::apply(
                                &mut io,
                                &threshold_update(Some(np), Some(nr), Some(nd)),
                                false,
                            )
                            .unwrap();
                            assert!(result.error.is_none());
                            assert_eq!(io.0.state(), State([np, nr, nd, 0xa303]));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn haptics_preserves_every_unrelated_control_bit_and_register() {
    for flags in [0x0002, 0xffff, 0x8003, 0x0000, 0x0100] {
        for on in [true, false] {
            let mut io = Protocol(Pad::new([150, 125, 60, flags]));
            let before = io.0.registers.clone();
            let update = Update {
                thresholds: [None; 3],
                haptics: Some(on),
            };
            let report = elan::apply(&mut io, &update, false).unwrap();
            assert!(report.error.is_none());
            let desired = (flags & !0x100) | if on { 0x100 } else { 0 };
            assert_eq!(io.0.registers[&0x03a1], desired);
            for (reg, value) in before {
                if reg != 0x03a1 {
                    assert_eq!(io.0.registers[&reg], value);
                }
            }
        }
    }
}

#[test]
fn no_op_issues_no_parameter_writes() {
    let mut io = Protocol(Pad::new([120, 95, 60, 0x100]));
    let report = elan::apply(&mut io, &threshold_update(Some(120), Some(95), None), false).unwrap();
    assert_eq!(report.status, "unchanged");
    assert!(io.0.writes.is_empty());
}

#[test]
fn failures_at_each_write_before_and_after_mutation_restore_original_in_safe_order() {
    for initial in [[150, 125, 125, 0xa303], [120, 95, 60, 0xa303]] {
        let target = if initial[0] == 150 {
            [120, 95, 60]
        } else {
            [192, 154, 125]
        };
        for failure in 1..=3 {
            for after in [true, false] {
                let mut io = Protocol(Pad::new(initial));
                io.0.fail_writes = vec![failure];
                io.0.fail_after_apply = after;
                let report = elan::apply(
                    &mut io,
                    &threshold_update(Some(target[0]), Some(target[1]), Some(target[2])),
                    false,
                )
                .unwrap();
                assert_eq!(report.status, "failed");
                assert_eq!(report.attempted.len(), failure);
                assert_eq!(report.verified.len(), failure - 1);
                assert_eq!(report.rollback.unwrap().status, "restored");
                assert_eq!(io.0.state(), State(initial));
            }
        }
    }
}

#[test]
fn readback_mismatch_triggers_verified_rollback() {
    let mut io = Protocol(Pad::new([150, 125, 60, 0x100]));
    io.0.corrupt_write = Some(1);
    let report = elan::apply(&mut io, &threshold_update(Some(120), Some(95), None), false).unwrap();
    assert_eq!(report.error.unwrap().kind, "verification");
    assert_eq!(report.rollback.unwrap().status, "restored");
    assert_eq!(io.0.state(), State([150, 125, 60, 0x100]));
}

#[test]
fn rollback_failure_and_unknown_state_are_not_reported_as_success() {
    let mut io = Protocol(Pad::new([150, 125, 125, 0x100]));
    io.0.fail_writes = vec![3, 4];
    let report = elan::apply(
        &mut io,
        &threshold_update(Some(120), Some(95), Some(60)),
        false,
    )
    .unwrap();
    assert_eq!(report.status, "failed");
    let rollback = report.rollback.unwrap();
    assert_eq!(rollback.status, "incomplete");
    assert!(rollback.error.is_some());
    assert_eq!(rollback.attempted.len(), 1);
    assert!(rollback.verified.is_empty());
    assert!(report.observed_after.is_some());

    let mut io = Protocol(Pad::new([150, 125, 125, 0x100]));
    io.0.fail_reads_after_write = true;
    let report = elan::apply(
        &mut io,
        &threshold_update(Some(120), Some(95), Some(60)),
        false,
    )
    .unwrap();
    assert_eq!(report.status, "failed");
    assert_eq!(report.rollback.unwrap().status, "unknown");
    assert!(report.observed_after.is_none());
    assert_eq!(
        io.0.writes.len(),
        1,
        "no blind rollback writes when state is unreadable"
    );
}
