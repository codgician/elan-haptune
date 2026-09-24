use crate::{
    error::{Error, Result},
    protocol::{FeatureIo, Protocol},
};
use serde::Serialize;

pub const REGS: [u16; 4] = [0x03a2, 0x03a3, 0x03a4, 0x03a1];
const NAMES: [&str; 4] = [
    "press_threshold",
    "release_threshold",
    "drag_release_threshold",
    "haptics",
];
const HAPTICS: u16 = 0x0100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State(pub [u16; 4]);

impl State {
    pub fn read<T: FeatureIo>(io: &mut Protocol<T>) -> Result<Self> {
        let mut values = [0; 4];
        for (value, reg) in values.iter_mut().zip(REGS) {
            *value = io.read(reg)?;
        }
        Ok(Self(values))
    }
    pub fn settings(self) -> serde_json::Value {
        serde_json::json!({"press_threshold": self.0[0], "release_threshold": self.0[1], "drag_release_threshold": self.0[2], "haptics": if self.0[3] & HAPTICS != 0 { "on" } else { "off" }})
    }
    fn validate(self) -> Result<()> {
        if self.0[1] >= self.0[0] || self.0[2] >= self.0[0] {
            return Err(Error::invalid(
                "both release thresholds must be strictly below press threshold",
            ));
        }
        Ok(())
    }
}

#[derive(Default, Debug, Clone)]
pub struct Update {
    pub thresholds: [Option<u16>; 3],
    pub haptics: Option<bool>,
}

impl Update {
    pub fn validate(&self) -> Result<()> {
        if self.thresholds.iter().all(Option::is_none) && self.haptics.is_none() {
            return Err(Error::invalid("set requires at least one setting"));
        }
        Ok(())
    }
    pub fn target(&self, before: State) -> Result<State> {
        self.validate()?;
        before.validate().map_err(|e| {
            Error::unsupported(format!("current thresholds cannot be safely updated: {e}"))
        })?;
        let mut after = before;
        for (i, value) in self.thresholds.iter().enumerate() {
            if let Some(v) = value {
                after.0[i] = *v;
            }
        }
        if let Some(on) = self.haptics {
            after.0[3] = (after.0[3] & !HAPTICS) | if on { HAPTICS } else { 0 };
        }
        after.validate()?;
        Ok(after)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub setting: &'static str,
    pub before_raw: u16,
    pub after_raw: u16,
    #[serde(skip)]
    index: usize,
}

// For valid endpoints, increasing press first and decreasing it last keeps both
// release thresholds below press at every intermediate state. No release coupling.
fn plan(before: State, after: State) -> Result<Vec<Change>> {
    before.validate()?;
    after.validate()?;
    let mut order = Vec::new();
    if after.0[3] & HAPTICS == 0 {
        order.push(3);
    }
    if after.0[0] > before.0[0] {
        order.push(0);
    }
    order.extend([1, 2]);
    if after.0[0] <= before.0[0] {
        order.push(0);
    }
    if after.0[3] & HAPTICS != 0 {
        order.push(3);
    }
    Ok(order
        .into_iter()
        .filter(|&i| before.0[i] != after.0[i])
        .map(|i| Change {
            setting: NAMES[i],
            before_raw: before.0[i],
            after_raw: after.0[i],
            index: i,
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct Rollback {
    pub status: &'static str,
    pub attempted: Vec<Change>,
    pub verified: Vec<Change>,
    pub error: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct UpdateReport {
    pub status: &'static str,
    pub before: serde_json::Value,
    pub requested: serde_json::Value,
    pub observed_after: Option<serde_json::Value>,
    pub planned: Vec<Change>,
    pub attempted: Vec<Change>,
    pub verified: Vec<Change>,
    pub error: Option<Error>,
    pub rollback: Option<Rollback>,
}

pub fn apply<T: FeatureIo>(
    io: &mut Protocol<T>,
    update: &Update,
    dry_run: bool,
) -> Result<UpdateReport> {
    update.validate()?;
    let before = State::read(io)?;
    let target = update.target(before)?;
    let planned = plan(before, target)?;
    let mut report = UpdateReport {
        status: if dry_run {
            "dry_run"
        } else if planned.is_empty() {
            "unchanged"
        } else {
            "applied"
        },
        before: before.settings(),
        requested: target.settings(),
        observed_after: Some(before.settings()),
        planned,
        attempted: Vec::new(),
        verified: Vec::new(),
        error: None,
        rollback: None,
    };
    if dry_run || report.planned.is_empty() {
        return Ok(report);
    }
    let mut expected = before;
    for change in report.planned.clone() {
        let result = (|| {
            if State::read(io)? != expected {
                return Err(Error::new(
                    1,
                    "concurrent_change",
                    "settings changed outside this process; stopping",
                ));
            }
            report.attempted.push(change.clone());
            io.write_verified(REGS[change.index], change.after_raw)?;
            expected.0[change.index] = change.after_raw;
            report.verified.push(change);
            Ok(())
        })();
        if let Err(error) = result {
            report.error = Some(error);
            break;
        }
    }
    if report.error.is_none() {
        match State::read(io) {
            Ok(state) if state == target => {
                report.observed_after = Some(state.settings());
                return Ok(report);
            }
            Ok(_) => {
                report.error = Some(Error::new(
                    1,
                    "verification",
                    "final settings differ from requested settings",
                ))
            }
            Err(e) => report.error = Some(e),
        }
    }
    report.status = "failed";
    report.observed_after = None;
    if report.attempted.is_empty() {
        report.observed_after = State::read(io).ok().map(State::settings);
        return Ok(report);
    }
    let mut rollback = Rollback {
        status: "incomplete",
        attempted: Vec::new(),
        verified: Vec::new(),
        error: None,
    };
    let restore = (|| {
        let current = State::read(io)?;
        for i in 0..4 {
            if current.0[i] != before.0[i] && !report.attempted.iter().any(|c| c.index == i) {
                return Err(Error::new(
                    1,
                    "rollback",
                    "an untouched setting changed; refusing to overwrite external changes",
                ));
            }
        }
        // Never overwrite externally modified shared bits, even during rollback.
        if (current.0[3] ^ before.0[3]) & !HAPTICS != 0 {
            return Err(Error::new(
                1,
                "rollback",
                "unrelated control bits changed; rollback stopped",
            ));
        }
        let changes = plan(current, before).map_err(|e| {
            Error::new(
                1,
                "rollback",
                format!("cannot establish safe rollback order: {e}"),
            )
        })?;
        let mut expected = current;
        for change in changes {
            if State::read(io)? != expected {
                return Err(Error::new(
                    1,
                    "rollback",
                    "settings changed during rollback",
                ));
            }
            rollback.attempted.push(change.clone());
            io.write_verified(REGS[change.index], change.after_raw)?;
            expected.0[change.index] = change.after_raw;
            rollback.verified.push(change);
        }
        Ok(())
    })();
    rollback.error = restore.err();
    match State::read(io) {
        Ok(actual) => {
            report.observed_after = Some(actual.settings());
            if actual == before {
                rollback.status = "restored";
            }
        }
        Err(e) => {
            rollback.status = "unknown";
            if rollback.error.is_none() {
                rollback.error = Some(e);
            }
        }
    }
    report.rollback = Some(rollback);
    Ok(report)
}

pub fn capabilities() -> serde_json::Value {
    serde_json::json!({
        "units": "device_raw", "firmware_ranges": null,
        "cli_threshold_bounds": null,
        "threshold_encoding": {"type": "u16", "min": 0, "max": 65535},
        "constraints": "both releases strictly below press; unspecified values preserved",
        "haptics": {"read": true, "write": true, "evidence": "vendor bit 8; not hardware-validated by this project"},
        "haptic_level": {"read": false, "write": false, "reason": "physical interpretation and safe target behavior unverified"}
    })
}
