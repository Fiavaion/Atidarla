//! Capability flags for platform-exclusive operators (PRD A7, A19).
//!
//! A flag is a `+`-joined conjunction of tokens, written the way the PRD
//! writes them: `win+nvidia`, `mac`, `win+sdk:kinect`. Every token must hold
//! on the running machine for the flag to be satisfied:
//!
//! - an OS token: `win`, `mac`, `linux` (at most one);
//! - a GPU vendor token: `nvidia`, `amd`, `intel`, `apple` (at most one);
//! - any number of `sdk:<name>` tokens for optional runtimes.
//!
//! Each operator that needs a flag also declares a [`Fallback`], so the
//! engine always knows what to do on other platforms: route the work to a
//! LAN/cloud host, run degraded, substitute another operator, or hide the
//! operator with an explanation shown in the UI.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::platform::{GpuVendor, Os, PlatformInfo};

/// A conjunction of platform requirements, e.g. `win+nvidia`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CapabilityFlag {
    pub os: Option<Os>,
    pub vendor: Option<GpuVendor>,
    pub sdks: BTreeSet<String>,
}

/// Error parsing a capability flag string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityParseError {
    #[error("capability flag is empty")]
    Empty,
    #[error("unknown capability token `{0}`")]
    UnknownToken(String),
    #[error("capability flag names more than one {kind}: `{flag}`")]
    Conflict { kind: &'static str, flag: String },
    #[error("empty SDK name in `{0}`")]
    EmptySdk(String),
}

impl CapabilityFlag {
    /// True when every token of the flag holds on `platform`.
    pub fn is_satisfied_by(&self, platform: &PlatformInfo) -> bool {
        self.os.is_none_or(|os| os == platform.os)
            && self
                .vendor
                .is_none_or(|vendor| vendor == platform.gpu_vendor)
            && self.sdks.iter().all(|sdk| platform.sdks.contains(sdk))
    }
}

impl FromStr for CapabilityFlag {
    type Err = CapabilityParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(CapabilityParseError::Empty);
        }
        let mut flag = CapabilityFlag::default();
        for token in s.split('+').map(str::trim) {
            if let Some(sdk) = token.strip_prefix("sdk:") {
                if sdk.is_empty() {
                    return Err(CapabilityParseError::EmptySdk(s.to_owned()));
                }
                flag.sdks.insert(sdk.to_owned());
                continue;
            }
            let os = match token {
                "win" => Some(Os::Windows),
                "mac" => Some(Os::MacOs),
                "linux" => Some(Os::Linux),
                _ => None,
            };
            if let Some(os) = os {
                if flag.os.replace(os).is_some() {
                    return Err(CapabilityParseError::Conflict {
                        kind: "OS",
                        flag: s.to_owned(),
                    });
                }
                continue;
            }
            let vendor = match token {
                "nvidia" => GpuVendor::Nvidia,
                "amd" => GpuVendor::Amd,
                "intel" => GpuVendor::Intel,
                "apple" => GpuVendor::Apple,
                other => return Err(CapabilityParseError::UnknownToken(other.to_owned())),
            };
            if flag.vendor.replace(vendor).is_some() {
                return Err(CapabilityParseError::Conflict {
                    kind: "GPU vendor",
                    flag: s.to_owned(),
                });
            }
        }
        Ok(flag)
    }
}

impl fmt::Display for CapabilityFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut tokens: Vec<String> = Vec::new();
        if let Some(os) = self.os {
            tokens.push(os.token().to_owned());
        }
        if let Some(vendor) = self.vendor {
            tokens.push(vendor.token().to_owned());
        }
        tokens.extend(self.sdks.iter().map(|sdk| format!("sdk:{sdk}")));
        f.write_str(&tokens.join("+"))
    }
}

impl Serialize for CapabilityFlag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CapabilityFlag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What an operator does where its capability flag is not satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fallback {
    /// Run the work on a LAN or cloud AI Host via the compute router.
    RouteRemote { explanation: String },
    /// Run locally at reduced quality or rate (e.g. interactive-tier AI).
    Degrade { explanation: String },
    /// Replace with another operator type (e.g. Spout → Syphon).
    Substitute { op: String, explanation: String },
    /// Hide the operator; the explanation is shown where it would appear.
    Hide { explanation: String },
}

impl Fallback {
    /// User-facing explanation of the fallback.
    pub fn explanation(&self) -> &str {
        match self {
            Fallback::RouteRemote { explanation }
            | Fallback::Degrade { explanation }
            | Fallback::Substitute { explanation, .. }
            | Fallback::Hide { explanation } => explanation,
        }
    }
}

/// A capability flag together with its mandatory fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    pub flag: CapabilityFlag,
    pub fallback: Fallback,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::PlatformInfo;

    fn platform(os: Os, vendor: GpuVendor) -> PlatformInfo {
        PlatformInfo {
            os,
            gpu_vendor: vendor,
            ..PlatformInfo::host()
        }
    }

    #[test]
    fn parses_and_round_trips_prd_flags() {
        for s in [
            "win+nvidia",
            "mac",
            "win+sdk:kinect",
            "nvidia",
            "linux+amd+sdk:a+sdk:b",
        ] {
            let flag: CapabilityFlag = s.parse().unwrap();
            assert_eq!(flag.to_string(), s);
        }
    }

    #[test]
    fn normalises_token_order() {
        let flag: CapabilityFlag = "sdk:kinect + nvidia + win".parse().unwrap();
        assert_eq!(flag.to_string(), "win+nvidia+sdk:kinect");
    }

    #[test]
    fn rejects_bad_flags() {
        assert_eq!(
            "".parse::<CapabilityFlag>(),
            Err(CapabilityParseError::Empty)
        );
        assert!(
            matches!("win+cuda".parse::<CapabilityFlag>(), Err(CapabilityParseError::UnknownToken(t)) if t == "cuda")
        );
        assert!(matches!(
            "win+mac".parse::<CapabilityFlag>(),
            Err(CapabilityParseError::Conflict { .. })
        ));
        assert!(matches!(
            "nvidia+amd".parse::<CapabilityFlag>(),
            Err(CapabilityParseError::Conflict { .. })
        ));
        assert!(matches!(
            "sdk:".parse::<CapabilityFlag>(),
            Err(CapabilityParseError::EmptySdk(_))
        ));
    }

    #[test]
    fn win_nvidia_is_satisfied_only_on_windows_nvidia() {
        let flag: CapabilityFlag = "win+nvidia".parse().unwrap();
        assert!(flag.is_satisfied_by(&platform(Os::Windows, GpuVendor::Nvidia)));
        assert!(!flag.is_satisfied_by(&platform(Os::Windows, GpuVendor::Amd)));
        assert!(!flag.is_satisfied_by(&platform(Os::MacOs, GpuVendor::Apple)));
        assert!(!flag.is_satisfied_by(&platform(Os::Linux, GpuVendor::Nvidia)));
    }

    #[test]
    fn sdk_tokens_require_detected_sdk() {
        let flag: CapabilityFlag = "win+sdk:kinect".parse().unwrap();
        let without = platform(Os::Windows, GpuVendor::Nvidia);
        assert!(!flag.is_satisfied_by(&without));
        assert!(flag.is_satisfied_by(&without.with_sdk("kinect")));
    }

    #[test]
    fn serde_uses_string_form() {
        let req = CapabilityRequirement {
            flag: "win+nvidia".parse().unwrap(),
            fallback: Fallback::RouteRemote {
                explanation: "Runs on a LAN or cloud host.".into(),
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["flag"], "win+nvidia");
        assert_eq!(json["fallback"]["kind"], "route_remote");
        let back: CapabilityRequirement = serde_json::from_value(json).unwrap();
        assert_eq!(back, req);
    }
}
