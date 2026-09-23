use crate::error::{Error, Result};

pub const REPORT: u8 = 0x0d;

// The only transport seam: tests exercise the real report codec and update engine.
pub trait FeatureIo {
    fn set_feature(&mut self, report: &mut [u8; 5]) -> Result<usize>;
    fn get_feature(&mut self, report: &mut [u8; 5]) -> Result<usize>;
    fn settle(&self) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

pub struct Protocol<T>(pub T);

impl<T: FeatureIo> Protocol<T> {
    pub fn read(&mut self, reg: u16) -> Result<u16> {
        let [lo, hi] = reg.to_le_bytes();
        // SET_FEATURE here selects a query; it does not set a parameter.
        let mut request = [REPORT, 0x05, 0x03, lo, hi];
        let count = self
            .0
            .set_feature(&mut request)
            .map_err(|e| e.context(format!("query request 0x{reg:04x}")))?;
        exact_length(count, "query")?;
        let mut response = [REPORT, 0, 0, 0, 0];
        let count = self
            .0
            .get_feature(&mut response)
            .map_err(|e| e.context(format!("query response 0x{reg:04x}")))?;
        decode(reg, &response[..count.min(5)], count)
    }

    pub fn write_verified(&mut self, reg: u16, value: u16) -> Result<()> {
        let [rl, rh] = reg.to_le_bytes();
        let [vl, vh] = value.to_le_bytes();
        let mut request = [REPORT, rl, rh, vl, vh];
        let sent = self.0.set_feature(&mut request);
        self.0.settle();
        // Even an ioctl error can follow an applied write. Always attempt readback.
        let actual = self.read(reg);
        exact_length(
            sent.map_err(|e| e.context(format!("write 0x{reg:04x}={value}")))?,
            "parameter write",
        )?;
        let actual = actual?;
        if actual != value {
            return Err(Error::new(
                1,
                "verification",
                format!("0x{reg:04x}: requested {value}, read back {actual}"),
            ));
        }
        Ok(())
    }

    pub fn check_identity(&mut self) -> Result<()> {
        let first = self.read(0x0101)?;
        let second = self.read(0x0103)?;
        if (first, second) != (0x0130, 0x1300) {
            return Err(Error::unsupported(format!(
                "unrecognized protocol identity 0x{first:04x}/0x{second:04x}; expected 0x0130/0x1300"
            )));
        }
        Ok(())
    }
}

fn exact_length(count: usize, operation: &str) -> Result<()> {
    if count != 5 {
        return Err(Error::protocol(format!(
            "{operation}: expected 5 bytes, got {count}"
        )));
    }
    Ok(())
}

fn decode(reg: u16, bytes: &[u8], count: usize) -> Result<u16> {
    exact_length(count, "query response")?;
    let [lo, hi] = reg.to_le_bytes();
    if bytes.len() != 5 || bytes[..3] != [REPORT, lo, hi] {
        return Err(Error::protocol(format!(
            "invalid report/register echo for 0x{reg:04x}: {bytes:02x?}"
        )));
    }
    Ok(u16::from_le_bytes([bytes[3], bytes[4]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decode_requires_exact_length_report_and_both_address_bytes() {
        assert_eq!(
            decode(0x03a2, &[13, 0xa2, 3, 0x34, 0x12], 5).unwrap(),
            0x1234
        );
        for (bytes, count) in [
            ([13, 0xa2, 3, 0, 0], 4),
            ([13, 0xa2, 3, 0, 0], 6),
            ([12, 0xa2, 3, 0, 0], 5),
            ([13, 0xa3, 3, 0, 0], 5),
            ([13, 0xa2, 4, 0, 0], 5),
        ] {
            assert!(decode(0x03a2, &bytes, count).is_err());
        }
    }
}
