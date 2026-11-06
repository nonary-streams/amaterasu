//! Pure validation of independently observed capture readiness.
use crate::model::Specification;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CaptureReadback {
    pub active: bool,
    pub width: u32,
    pub height: u32,
    pub refresh_numerator: u32,
    pub refresh_denominator: u32,
    pub hdr_enabled: bool,
}

impl CaptureReadback {
    pub(super) fn matches(self, specification: &Specification) -> bool {
        let Some(refresh) = u64::from(self.refresh_numerator)
            .checked_mul(1000)
            .and_then(|numerator| numerator.checked_add(u64::from(self.refresh_denominator) / 2))
            .and_then(|numerator| numerator.checked_div(u64::from(self.refresh_denominator)))
        else {
            return false;
        };
        self.active
            && self.width == specification.width
            && self.height == specification.height
            && refresh == u64::from(specification.refresh_millihz)
            && self.hdr_enabled == specification.hdr_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specification() -> Specification {
        Specification {
            identity: "00000000-0000-0000-0000-000000000001".into(),
            name: "Remote monitor".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 59940,
            hdr_enabled: true,
        }
    }

    #[test]
    fn capture_readiness_requires_observed_active_mode_refresh_and_color() {
        let expected = specification();
        let ready = CaptureReadback {
            active: true,
            width: 1920,
            height: 1080,
            refresh_numerator: 60000,
            refresh_denominator: 1001,
            hdr_enabled: true,
        };
        assert!(ready.matches(&expected));
        for invalid in [
            CaptureReadback {
                active: false,
                ..ready
            },
            CaptureReadback {
                width: 1280,
                ..ready
            },
            CaptureReadback {
                height: 720,
                ..ready
            },
            CaptureReadback {
                refresh_numerator: 60,
                refresh_denominator: 1,
                ..ready
            },
            CaptureReadback {
                refresh_denominator: 0,
                ..ready
            },
            CaptureReadback {
                hdr_enabled: false,
                ..ready
            },
        ] {
            assert!(
                !invalid.matches(&expected),
                "unexpected readiness: {invalid:?}"
            );
        }
        let sdr = Specification {
            hdr_enabled: false,
            ..expected
        };
        assert!(
            CaptureReadback {
                hdr_enabled: false,
                ..ready
            }
            .matches(&sdr)
        );
        assert!(!ready.matches(&sdr));
    }
}
