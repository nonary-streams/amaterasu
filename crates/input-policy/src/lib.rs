//! Typed input preferences shared by independently built product modules.
//! This library has no native input, runtime authority, or SDK dependencies.

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// An ordered flat list of source/destination virtual-key byte pairs.
///
/// The wire contract accepts exact integers in 0..=255, including reserved
/// values 0 and 255. Native delivery still requires the selected backend and
/// keyboard mode to support the resolved destination. Unlike the reference's
/// arbitrary integer parser, wider values are rejected, never byte-truncated.
/// The selected whole list replaces a lower scope's list; an explicit empty
/// list keeps only the built-in modifier pairs and the optional Right Alt alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct KeyBindings(Vec<u8>);

impl KeyBindings {
    pub const MAX_PAIRS: usize = 256;
    pub const MAX_VALUES: usize = Self::MAX_PAIRS * 2;

    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn from_flat(values: Vec<u8>) -> Result<Self, &'static str> {
        if values.len() > Self::MAX_VALUES || !values.len().is_multiple_of(2) {
            return Err("keybindings must contain at most 256 ordered source/destination pairs");
        }
        Ok(Self(values))
    }

    pub fn as_flat(&self) -> &[u8] {
        &self.0
    }

    /// Apply exactly one mapping. Built-in pairs were inserted before custom
    /// pairs in the reference; the Right Alt alias was inserted after them.
    pub fn resolve(&self, source: u8, right_alt_to_windows: bool) -> u8 {
        if matches!(source, 0x10..=0x12) {
            return host_modifier_identity(source);
        }
        if let Some(pair) = self
            .0
            .as_chunks::<2>()
            .0
            .iter()
            .find(|pair| pair[0] == source)
        {
            return pair[1];
        }
        if right_alt_to_windows && source == 0xa5 {
            0x5b
        } else {
            source
        }
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self(vec![0x10, 0xa0, 0x11, 0xa2, 0x12, 0xa4])
    }
}

/// Generic and left modifiers address the same native host key. This is an
/// ownership identity only; it never applies another custom mapping.
pub const fn host_modifier_identity(key: u8) -> u8 {
    match key {
        0x10 => 0xa0,
        0x11 => 0xa2,
        0x12 => 0xa4,
        key => key,
    }
}

impl<'de> Deserialize<'de> for KeyBindings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedPairs;

        impl<'de> Visitor<'de> for OrderedPairs {
            type Value = KeyBindings;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an even flat list of at most 512 exact virtual-key bytes")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                if sequence
                    .size_hint()
                    .is_some_and(|size| size > KeyBindings::MAX_VALUES)
                {
                    return Err(de::Error::custom("keybindings exceeds 256 pairs"));
                }
                let mut values = Vec::with_capacity(
                    sequence
                        .size_hint()
                        .unwrap_or(0)
                        .min(KeyBindings::MAX_VALUES),
                );
                while let Some(value) = sequence.next_element::<u8>()? {
                    if values.len() == KeyBindings::MAX_VALUES {
                        return Err(de::Error::custom("keybindings exceeds 256 pairs"));
                    }
                    values.push(value);
                }
                KeyBindings::from_flat(values).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_seq(OrderedPairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn builtins_first_duplicate_and_alias_have_reference_priority() {
        let custom: KeyBindings =
            serde_json::from_value(json!([0x10, 0x41, 0x41, 0x42, 0x41, 0x43, 0xa5, 0x44,]))
                .unwrap();
        assert_eq!(custom.resolve(0x10, false), 0xa0);
        assert_eq!(custom.resolve(0x11, false), 0xa2);
        assert_eq!(custom.resolve(0x12, false), 0xa4);
        assert_eq!(custom.resolve(0x41, false), 0x42);
        assert_eq!(custom.resolve(0xa5, true), 0x44);
        assert_eq!(KeyBindings::empty().resolve(0xa5, true), 0x5b);
        assert_eq!(KeyBindings::empty().resolve(0xa5, false), 0xa5);
    }

    #[test]
    fn mapping_is_single_and_empty_does_not_merge_lower_custom_values() {
        let custom: KeyBindings =
            serde_json::from_value(json!([0x41, 0x42, 0x42, 0x43, 0xa0, 0x44])).unwrap();
        assert_eq!(custom.resolve(0x41, false), 0x42);
        assert_eq!(custom.resolve(0x10, false), 0xa0);
        assert_eq!(custom.resolve(0xa0, false), 0x44);
        assert_eq!(KeyBindings::empty().resolve(0x41, false), 0x41);
        assert_eq!(
            serde_json::to_value(KeyBindings::empty()).unwrap(),
            json!([])
        );
        assert_eq!(
            serde_json::to_value(KeyBindings::default()).unwrap(),
            json!([16, 160, 17, 162, 18, 164])
        );
    }

    #[test]
    fn byte_boundaries_are_explicit_and_wider_values_never_truncate() {
        let boundaries: KeyBindings = serde_json::from_value(json!([0, 255, 255, 0])).unwrap();
        assert_eq!(boundaries.resolve(0, false), 255);
        assert_eq!(boundaries.resolve(255, false), 0);
        for invalid in [
            json!([-1, 0]),
            json!([0, 256]),
            json!([65535, 0]),
            json!([0.0, 1]),
            json!([true, 0]),
            json!(["0", 1]),
            json!([0]),
            json!({}),
            Value::Null,
        ] {
            assert!(
                serde_json::from_value::<KeyBindings>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn deserialization_and_construction_bound_allocation_and_pair_count() {
        assert!(serde_json::from_value::<KeyBindings>(json!(vec![0_u8; 512])).is_ok());
        assert!(serde_json::from_value::<KeyBindings>(json!(vec![0_u8; 514])).is_err());
        assert!(KeyBindings::from_flat(vec![0; 514]).is_err());
        assert!(KeyBindings::from_flat(vec![0]).is_err());
    }
}
