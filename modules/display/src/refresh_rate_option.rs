use serde::{Deserialize, Serialize};

use crate::model::Rational;

/// Describes why a manual refresh-rate string could not be parsed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualRefreshRateParseError {
    /// The input is not an ASCII decimal integer or fraction.
    InvalidFormat,
    /// The exact rational value does not fit in `u32` fields.
    OutOfRange,
}

/// Parses the `dd_manual_refresh_rate` string companion.
///
/// Surrounding whitespace is ignored. An empty value is unspecified, while a
/// non-empty decimal is returned exactly as a reduced rational. This parser
/// does not validate whether the resulting rate is usable by the display.
pub fn parse_manual_refresh_rate(
    value: &str,
) -> Result<Option<Rational>, ManualRefreshRateParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    let rate = match value.split_once('.') {
        Some((integer_part, fractional_part)) => parse_decimal_rate(integer_part, fractional_part)?,
        None => parse_integer_rate(value)?,
    };

    Ok(Some(rate))
}

fn parse_integer_rate(value: &str) -> Result<Rational, ManualRefreshRateParseError> {
    if !is_ascii_decimal(value) {
        return Err(ManualRefreshRateParseError::InvalidFormat);
    }

    let integer_digits = value.trim_start_matches('0');
    let numerator = if integer_digits.is_empty() {
        0
    } else {
        integer_digits
            .parse::<u32>()
            .map_err(|_| ManualRefreshRateParseError::OutOfRange)?
    };

    Ok(Rational {
        numerator,
        denominator: 1,
    })
}

fn parse_decimal_rate(
    integer_part: &str,
    fractional_part: &str,
) -> Result<Rational, ManualRefreshRateParseError> {
    if !is_ascii_decimal(integer_part) || !is_ascii_decimal(fractional_part) {
        return Err(ManualRefreshRateParseError::InvalidFormat);
    }

    let integer_rate = parse_integer_rate(integer_part)?;
    let significant_fraction = fractional_part.trim_end_matches('0');
    if significant_fraction.is_empty() {
        return Ok(integer_rate);
    }

    // A reduced denominator that fits u32 has only factors 2 and 5, so its
    // terminating decimal representation needs at most 31 fractional digits.
    if significant_fraction.len() > 31 {
        return Err(ManualRefreshRateParseError::OutOfRange);
    }

    let fractional_numerator = significant_fraction
        .parse::<u128>()
        .map_err(|_| ManualRefreshRateParseError::OutOfRange)?;
    let fractional_denominator = 10_u128
        .checked_pow(significant_fraction.len() as u32)
        .ok_or(ManualRefreshRateParseError::OutOfRange)?;
    let common_divisor = greatest_common_divisor(fractional_numerator, fractional_denominator);
    let fractional_numerator = fractional_numerator / common_divisor;
    let fractional_denominator = fractional_denominator / common_divisor;
    let denominator = u32::try_from(fractional_denominator)
        .map_err(|_| ManualRefreshRateParseError::OutOfRange)?;

    let numerator = u64::from(integer_rate.numerator)
        .checked_mul(u64::from(denominator))
        .and_then(|scaled_integer| {
            u64::try_from(fractional_numerator)
                .ok()
                .and_then(|fraction| scaled_integer.checked_add(fraction))
        })
        .and_then(|numerator| u32::try_from(numerator).ok())
        .ok_or(ManualRefreshRateParseError::OutOfRange)?;

    Ok(Rational {
        numerator,
        denominator,
    })
}

fn is_ascii_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }

    left
}

/// Describes how a configuration chooses a target display refresh rate.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshRateOption {
    /// Use the client's requested FPS as the target refresh rate.
    #[default]
    Auto,
    /// Do not perform additional refresh-rate configuration.
    Disabled,
    /// Use the companion `dd_manual_refresh_rate` setting.
    Manual,
    /// Prefer the highest refresh rate available for the selected resolution.
    PreferHighest,
}

impl RefreshRateOption {
    /// Parses a legacy configuration token, falling back to Disabled if unknown.
    pub fn from_legacy_token(token: &str) -> Self {
        match token {
            "auto" => Self::Auto,
            "disabled" => Self::Disabled,
            "manual" => Self::Manual,
            "prefer_highest" => Self::PreferHighest,
            _ => Self::Disabled,
        }
    }

    /// Returns whether the client FPS is active as the target refresh rate.
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::{ManualRefreshRateParseError, RefreshRateOption, parse_manual_refresh_rate};
    use crate::model::Rational;

    #[test]
    fn defaults_to_auto_and_serializes_default_token() {
        assert_eq!(RefreshRateOption::default(), RefreshRateOption::Auto);
        assert_eq!(
            serde_json::to_string(&RefreshRateOption::default()).unwrap(),
            "\"auto\""
        );
    }

    #[test]
    fn parses_and_serializes_exact_tokens() {
        let options = [
            ("auto", RefreshRateOption::Auto),
            ("disabled", RefreshRateOption::Disabled),
            ("manual", RefreshRateOption::Manual),
            ("prefer_highest", RefreshRateOption::PreferHighest),
        ];

        for (token, option) in options {
            assert_eq!(
                serde_json::from_str::<RefreshRateOption>(&format!("\"{token}\"")).unwrap(),
                option
            );
            assert_eq!(
                serde_json::to_string(&option).unwrap(),
                format!("\"{token}\"")
            );
            assert_eq!(RefreshRateOption::from_legacy_token(token), option);
        }
    }

    #[test]
    fn unknown_legacy_tokens_disable_and_automatic_is_only_auto() {
        assert_eq!(
            RefreshRateOption::from_legacy_token("future_option"),
            RefreshRateOption::Disabled
        );
        assert!(RefreshRateOption::Auto.is_automatic());
        for option in [
            RefreshRateOption::Disabled,
            RefreshRateOption::Manual,
            RefreshRateOption::PreferHighest,
        ] {
            assert!(!option.is_automatic());
        }
    }

    #[test]
    fn rejects_non_string_json_values() {
        for value in ["null", "true", "42", "{}", "[]"] {
            assert!(
                serde_json::from_str::<RefreshRateOption>(value).is_err(),
                "unexpectedly accepted {value}"
            );
        }
    }

    #[test]
    fn manual_refresh_rate_blank_input_is_unspecified() {
        assert_eq!(parse_manual_refresh_rate(""), Ok(None));
        assert_eq!(parse_manual_refresh_rate(" \t\r\n "), Ok(None));
        assert_eq!(parse_manual_refresh_rate("\u{2003}"), Ok(None));
    }

    #[test]
    fn manual_refresh_rate_parses_integers() {
        assert_eq!(
            parse_manual_refresh_rate("120"),
            Ok(Some(Rational {
                numerator: 120,
                denominator: 1,
            }))
        );
        assert_eq!(
            parse_manual_refresh_rate(" 59 "),
            Ok(Some(Rational {
                numerator: 59,
                denominator: 1,
            }))
        );
    }

    #[test]
    fn manual_refresh_rate_preserves_decimal_exactness() {
        assert_eq!(
            parse_manual_refresh_rate("59.95"),
            Ok(Some(Rational {
                numerator: 1199,
                denominator: 20,
            }))
        );
        assert_eq!(
            parse_manual_refresh_rate("0.000000001"),
            Ok(Some(Rational {
                numerator: 1,
                denominator: 1_000_000_000,
            }))
        );
        assert_eq!(
            parse_manual_refresh_rate("0.0000000004656612873077392578125"),
            Ok(Some(Rational {
                numerator: 1,
                denominator: 2_147_483_648,
            }))
        );
    }

    #[test]
    fn manual_refresh_rate_normalizes_leading_and_trailing_zeroes() {
        assert_eq!(
            parse_manual_refresh_rate("00120.500000"),
            Ok(Some(Rational {
                numerator: 241,
                denominator: 2,
            }))
        );
        assert_eq!(
            parse_manual_refresh_rate("00042.000"),
            Ok(Some(Rational {
                numerator: 42,
                denominator: 1,
            }))
        );
    }

    #[test]
    fn manual_refresh_rate_rejects_malformed_input() {
        for value in [
            "+1", "-1", "1,5", "1_000", "1e2", "1.", ".5", "1..5", "1.2.3", "1 .5", "1. 5", "１２",
            "NaN",
        ] {
            assert_eq!(
                parse_manual_refresh_rate(value),
                Err(ManualRefreshRateParseError::InvalidFormat),
                "unexpected parse result for {value:?}"
            );
        }
    }

    #[test]
    fn manual_refresh_rate_rejects_values_outside_u32_rational_range() {
        for value in ["4294967296", "4294967295.5", "0.0000000001"] {
            assert_eq!(
                parse_manual_refresh_rate(value),
                Err(ManualRefreshRateParseError::OutOfRange),
                "unexpected parse result for {value:?}"
            );
        }
    }

    #[test]
    fn manual_refresh_rate_accepts_zero() {
        assert_eq!(
            parse_manual_refresh_rate("0"),
            Ok(Some(Rational {
                numerator: 0,
                denominator: 1,
            }))
        );
        assert_eq!(
            parse_manual_refresh_rate("000.0000"),
            Ok(Some(Rational {
                numerator: 0,
                denominator: 1,
            }))
        );
    }
}
