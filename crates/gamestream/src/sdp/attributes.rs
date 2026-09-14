use super::{Error, syntax::decimal};
use std::collections::BTreeMap;

pub(super) struct Attributes<'a>(pub BTreeMap<&'a str, &'a str>);

impl Attributes<'_> {
    pub fn required(&mut self, name: &'static str) -> Result<u32, Error> {
        self.optional(name)?.ok_or(Error::Missing(name))
    }

    pub fn optional(&mut self, name: &'static str) -> Result<Option<u32>, Error> {
        self.0
            .remove(name)
            .map(|value| decimal(value).ok_or(Error::Invalid(name)))
            .transpose()
    }

    pub fn default(&mut self, name: &'static str, default: u32) -> Result<u32, Error> {
        Ok(self.optional(name)?.unwrap_or(default))
    }

    pub fn require_value(&mut self, name: &'static str, expected: u32) -> Result<(), Error> {
        let value = self.required(name)?;
        if value != expected {
            return Err(Error::Unsupported(name));
        }
        Ok(())
    }

    pub fn default_value(&mut self, name: &'static str, expected: u32) -> Result<(), Error> {
        if self.default(name, expected)? != expected {
            return Err(Error::Unsupported(name));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<usize, Error> {
        // Known non-negotiating hints can be ignored, but still require unsigned syntax.
        for name in [
            "x-nv-video[0].timeoutLengthMs",
            "x-nv-video[0].framesWithInvalidRefThreshold",
            "x-nv-vqos[0].videoQualityScoreUpdateTime",
            "x-nv-vqos[0].qosTrafficType",
            "x-nv-aqos.qosTrafficType",
        ] {
            self.optional(name)?;
        }
        for name in self.0.keys() {
            // Unknown extension namespaces are bounded and ignored. New parameters
            // in known media/security families require an intentional implementation.
            if !name.starts_with("x-")
                || [
                    "x-nv-video",
                    "x-nv-vqos",
                    "x-nv-audio",
                    "x-nv-aqos",
                    "x-ss-video",
                    "x-ss-general.encryption",
                    "x-nv-general",
                    "x-nv-ri",
                ]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                return Err(Error::Unsupported("unrecognized critical attribute"));
            }
        }
        Ok(self.0.len())
    }
}

pub(super) fn bounded(value: u32, min: u32, max: u32, name: &'static str) -> Result<u32, Error> {
    if !(min..=max).contains(&value) {
        return Err(Error::Unsupported(name));
    }
    Ok(value)
}
