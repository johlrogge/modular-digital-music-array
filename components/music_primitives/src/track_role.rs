use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TrackRoleError {
    #[error("Unknown track role: '{0}'")]
    Unknown(String),
}

/// DJ curation role for a track in a set.
///
/// Describes where in a set this track is best placed.
/// Multiple aliases are accepted on input (lowercase and kebab-case);
/// the canonical serialised form is PascalCase with no spaces.
///
/// # Examples
/// ```
/// # use music_primitives::{TrackRole, TrackRoleError};
/// let role: TrackRole = "build-up".parse()?;
/// assert_eq!(role, TrackRole::BuildUp);
/// assert_eq!(role.to_string(), "Build Up");
/// # Ok::<(), TrackRoleError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackRole {
    Opener,
    BuildUp,
    Peak,
    Banger,
    CoolDown,
    Closer,
    Filler,
}

impl TrackRole {
    /// Canonical (wire-format) string for this role.
    fn as_canonical(&self) -> &'static str {
        match self {
            TrackRole::Opener => "Opener",
            TrackRole::BuildUp => "BuildUp",
            TrackRole::Peak => "Peak",
            TrackRole::Banger => "Banger",
            TrackRole::CoolDown => "CoolDown",
            TrackRole::Closer => "Closer",
            TrackRole::Filler => "Filler",
        }
    }
}

impl FromStr for TrackRole {
    type Err = TrackRoleError;

    /// Parse from canonical PascalCase, lowercase, or kebab-case strings.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "opener" | "Opener" => Ok(TrackRole::Opener),
            "build-up" | "buildup" | "BuildUp" | "Build Up" => Ok(TrackRole::BuildUp),
            "peak" | "Peak" => Ok(TrackRole::Peak),
            "banger" | "Banger" => Ok(TrackRole::Banger),
            "cool-down" | "cooldown" | "CoolDown" | "Cool Down" => Ok(TrackRole::CoolDown),
            "closer" | "Closer" => Ok(TrackRole::Closer),
            "filler" | "Filler" => Ok(TrackRole::Filler),
            _ => Err(TrackRoleError::Unknown(s.to_string())),
        }
    }
}

impl fmt::Display for TrackRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackRole::Opener => write!(f, "Opener"),
            TrackRole::BuildUp => write!(f, "Build Up"),
            TrackRole::Peak => write!(f, "Peak"),
            TrackRole::Banger => write!(f, "Banger"),
            TrackRole::CoolDown => write!(f, "Cool Down"),
            TrackRole::Closer => write!(f, "Closer"),
            TrackRole::Filler => write!(f, "Filler"),
        }
    }
}

impl Serialize for TrackRole {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_canonical())
    }
}

impl<'de> Deserialize<'de> for TrackRole {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<TrackRole>().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json;

    #[rstest]
    #[case("opener", TrackRole::Opener)]
    #[case("Opener", TrackRole::Opener)]
    #[case("build-up", TrackRole::BuildUp)]
    #[case("buildup", TrackRole::BuildUp)]
    #[case("BuildUp", TrackRole::BuildUp)]
    #[case("Build Up", TrackRole::BuildUp)]
    #[case("peak", TrackRole::Peak)]
    #[case("Peak", TrackRole::Peak)]
    #[case("banger", TrackRole::Banger)]
    #[case("Banger", TrackRole::Banger)]
    #[case("cool-down", TrackRole::CoolDown)]
    #[case("cooldown", TrackRole::CoolDown)]
    #[case("CoolDown", TrackRole::CoolDown)]
    #[case("Cool Down", TrackRole::CoolDown)]
    #[case("closer", TrackRole::Closer)]
    #[case("Closer", TrackRole::Closer)]
    #[case("filler", TrackRole::Filler)]
    #[case("Filler", TrackRole::Filler)]
    fn parse_all_aliases(#[case] input: &str, #[case] expected: TrackRole) {
        assert_eq!(input.parse::<TrackRole>().unwrap(), expected);
    }

    #[test]
    fn unknown_role_errors() {
        assert!("DJ".parse::<TrackRole>().is_err());
        assert!("".parse::<TrackRole>().is_err());
    }

    #[rstest]
    #[case(TrackRole::Opener, "Opener")]
    #[case(TrackRole::BuildUp, "Build Up")]
    #[case(TrackRole::Peak, "Peak")]
    #[case(TrackRole::Banger, "Banger")]
    #[case(TrackRole::CoolDown, "Cool Down")]
    #[case(TrackRole::Closer, "Closer")]
    #[case(TrackRole::Filler, "Filler")]
    fn display_is_human_readable(#[case] role: TrackRole, #[case] expected: &str) {
        assert_eq!(role.to_string(), expected);
    }

    #[rstest]
    #[case(TrackRole::Opener, "\"Opener\"")]
    #[case(TrackRole::BuildUp, "\"BuildUp\"")]
    #[case(TrackRole::CoolDown, "\"CoolDown\"")]
    fn serializes_as_pascal_case(#[case] role: TrackRole, #[case] expected_json: &str) {
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, expected_json);
    }

    #[rstest]
    #[case(TrackRole::Opener)]
    #[case(TrackRole::BuildUp)]
    #[case(TrackRole::Peak)]
    #[case(TrackRole::Banger)]
    #[case(TrackRole::CoolDown)]
    #[case(TrackRole::Closer)]
    #[case(TrackRole::Filler)]
    fn serde_roundtrip(#[case] role: TrackRole) {
        let json = serde_json::to_string(&role).unwrap();
        let back: TrackRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }
}
