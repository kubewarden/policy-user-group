use crate::LOG_DRAIN;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use slog::info;

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub(crate) struct IDRange {
    pub min: i64,
    pub max: i64,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub(crate) struct RuleStrategy {
    pub rule: String,
    pub ranges: Vec<IDRange>,
    pub overwrite: bool,
}
impl RuleStrategy {
    pub fn is_valid_id(&self, id: i64) -> bool {
        for range in &self.ranges {
            if id >= range.min && id <= range.max {
                return true;
            }
        }
        false
    }
}
impl Default for RuleStrategy {
    fn default() -> Self {
        RuleStrategy {
            rule: "RunAsAny".to_string(),
            ranges: vec![],
            overwrite: false,
        }
    }
}

impl IDRange {
    fn range_min_less_than_max(&self) -> Result<()> {
        if self.min > self.max {
            return Err(anyhow!("range min value should be greater than max value"));
        }
        Ok(())
    }

    fn is_valid(&self) -> Result<()> {
        self.range_min_less_than_max()?;
        Ok(())
    }
}

fn valid_ranges(ranges: &[IDRange]) -> Result<()> {
    let violations: Vec<_> = ranges.iter().filter(|r| r.is_valid().is_err()).collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("Invalid ID range: {:?}", violations))
    }
}

fn valid_user_rule_settings(rule: &str, ranges: &[IDRange], overwrite: bool) -> Result<()> {
    match rule {
        "RunAsAny" | "MustRunAsNonRoot" => {
            if overwrite {
                Err(anyhow!("Cannot set overwrite field with {:?}", rule))
            } else {
                Ok(())
            }
        }
        "MustRunAs" => {
            if ranges.is_empty() {
                Err(anyhow!(
                    "Invalid run_as_user settings: Missing user ID range",
                ))
            } else {
                valid_ranges(ranges).map_err(|e| anyhow!("Invalid run_as_user settings: {:?}", e))
            }
        }
        _ => Err(anyhow!("Invalid run_as_user settings: invalid rule.")),
    }
}

fn valid_group_rule_settings(
    rule: &str,
    ranges: &[IDRange],
    overwrite: bool,
    prefix_error_message: &str,
) -> Result<()> {
    match (rule, overwrite) {
        ("MayRunAs", true) | ("RunAsAny", true) => {
            Err(anyhow!("Cannot set overwrite field with {:?}", rule))
        }
        ("RunAsAny", false) => Ok(()),
        ("MustRunAs", _) | ("MayRunAs", false) => {
            if ranges.is_empty() {
                Err(anyhow!("{}: missing ID range", prefix_error_message))
            } else {
                valid_ranges(ranges).map_err(|e| anyhow!("{}: {:?}", prefix_error_message, e))
            }
        }
        _ => Err(anyhow!("{}: Invalid rule.", prefix_error_message)),
    }
}
// Describe the settings your policy expects when
// loaded by the policy server.
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub(crate) struct Settings {
    pub run_as_user: RuleStrategy,
    pub run_as_group: RuleStrategy,
    pub supplemental_groups: RuleStrategy,
}

impl kubewarden::settings::Validatable for Settings {
    fn validate(&self) -> core::result::Result<(), String> {
        info!(LOG_DRAIN, "starting settings validation");

        match valid_user_rule_settings(
            &self.run_as_user.rule,
            &self.run_as_user.ranges,
            self.run_as_user.overwrite,
        ) {
            Ok(()) => {}
            Err(error) => {
                return Err(error.to_string());
            }
        };
        match valid_group_rule_settings(
            &self.run_as_group.rule,
            &self.run_as_group.ranges,
            self.run_as_group.overwrite,
            "Invalid run_as_group settings",
        ) {
            Ok(()) => {}
            Err(error) => {
                return Err(error.to_string());
            }
        }
        match valid_group_rule_settings(
            &self.supplemental_groups.rule,
            &self.supplemental_groups.ranges,
            self.supplemental_groups.overwrite,
            "Invalid supplemental_groups settings",
        ) {
            Ok(()) => {}
            Err(error) => {
                return Err(error.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use kubewarden_policy_sdk::settings::Validatable;

    #[test]
    fn validate_settings() -> Result<(), ()> {
        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
        };

        assert!(settings.validate().is_ok());
        Ok(())
    }

    #[test]
    fn validate_run_as_user_rule_values() -> Result<(), ()> {
        let allowed_rules_values = ["RunAsAny", "MustRunAs", "MustRunAsNonRoot"];
        for rule in allowed_rules_values {
            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
            };

            assert!(settings.validate().is_ok());
        }
        Ok(())
    }

    #[test]
    fn validate_run_as_groups_and_supplemental_groups_rule_values() -> Result<(), ()> {
        let allowed_rules_values = ["RunAsAny", "MustRunAs", "MayRunAs"];
        for rule in allowed_rules_values {
            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
            };

            assert!(settings.validate().is_ok());

            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 1010,
                    }],
                    ..Default::default()
                },
            };

            assert!(settings.validate().is_ok());
        }
        Ok(())
    }

    #[test]
    fn validate_run_as_user_rule_invalid_values() -> Result<(), ()> {
        let allowed_rules_values = ["MayRunAs", "foo", "1234", "!@#$@#"];
        for rule in allowed_rules_values {
            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
            };

            assert!(
                settings.validate().is_err(),
                "User rule should not accept '{}' as rule value",
                rule
            );
        }
        Ok(())
    }

    #[test]
    fn validate_run_as_group_rule_invalid_values() -> Result<(), ()> {
        let allowed_rules_values = ["MustRunAsNonRoot", "foo", "1234", "!@#$@#"];
        for rule in allowed_rules_values {
            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
            };

            assert!(
                settings.validate().is_err(),
                "Group rule should not accept '{}' as rule value",
                rule
            );

            let settings = Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from(rule),
                    ranges: vec![IDRange { min: 1, max: 10 }],
                    ..Default::default()
                },
            };

            assert!(
                settings.validate().is_err(),
                "Supplemental group rule should not accept '{}' as rule value",
                rule
            );
        }
        Ok(())
    }

    #[test]
    fn validate_id_range_min_should_be_greater_than_max() -> Result<(), ()> {
        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 11, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "RunAsUser minimum ID Range value should be less than the max value"
        );

        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 11, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "RunAsGroup minimum ID Range value should be less than the max value"
        );

        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange { min: 1, max: 10 }, IDRange { min: 11, max: 10 }],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "SupplementalGroups minimum ID Range value should be less than the max value"
        );
        Ok(())
    }

    #[test]
    fn one_range_is_necessary() -> Result<(), ()> {
        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "MustRunAs rule requires at least one ID range"
        );

        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "MustRunAs rule requires at least one ID range"
        );

        let settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange { min: 1, max: 10 }],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![],
                ..Default::default()
            },
        };

        assert!(
            settings.validate().is_err(),
            "MustRunAs rule requires at least one ID range"
        );

        Ok(())
    }

    #[test]
    fn id_should_be_valid_if_it_exists_in_ranges() -> Result<(), ()> {
        let rule_strategy = RuleStrategy {
            rule: String::from("MustRunAs"),
            ranges: vec![IDRange {
                min: 1000,
                max: 2000,
            }],
            ..Default::default()
        };
        let is_valid = rule_strategy.is_valid_id(1000);
        assert!(is_valid);
        let rule_strategy = RuleStrategy {
            rule: String::from("MustRunAs"),
            ranges: vec![
                IDRange {
                    min: 1000,
                    max: 2000,
                },
                IDRange {
                    min: 500,
                    max: 1500,
                },
            ],
            ..Default::default()
        };
        let is_valid = rule_strategy.is_valid_id(1000);
        assert!(is_valid);
        let is_valid = rule_strategy.is_valid_id(1499);
        assert!(is_valid);

        let rule_strategy = RuleStrategy {
            rule: String::from("MustRunAs"),
            ranges: vec![IDRange {
                min: 1000,
                max: 2000,
            }],
            ..Default::default()
        };
        let is_valid = rule_strategy.is_valid_id(999);
        assert_eq!(is_valid, false);
        let is_valid = rule_strategy.is_valid_id(2001);
        assert_eq!(is_valid, false);

        let rule_strategy = RuleStrategy {
            rule: String::from("MustRunAs"),
            ranges: vec![
                IDRange {
                    min: 1000,
                    max: 2000,
                },
                IDRange {
                    min: 500,
                    max: 1500,
                },
            ],
            ..Default::default()
        };
        let is_valid = rule_strategy.is_valid_id(2001);
        assert_eq!(is_valid, false);
        let is_valid = rule_strategy.is_valid_id(499);
        assert_eq!(is_valid, false);
        let is_valid = rule_strategy.is_valid_id(999);
        assert!(is_valid);
        let is_valid = rule_strategy.is_valid_id(1501);
        assert!(is_valid);

        Ok(())
    }
    #[test]
    fn overwrite_settings_test() -> Result<(), String> {
        let mut settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
        };
        settings.validate()?;

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
        };
        assert!(
            settings.validate().is_err(),
            "overwrite field should be allowed only with the 'MustRunAs' rule"
        );

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
        };
        settings.validate()?;

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("MayRunAs"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
        };
        assert!(
            settings.validate().is_err(),
            "overwrite field should be allowed only with the 'MustRunAs' rule"
        );

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
        };
        assert!(
            settings.validate().is_err(),
            "overwrite field should be allowed only with the 'MustRunAs' rule"
        );

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MustRunAs"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
        };
        settings.validate()?;

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("MayRunAs"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
        };
        assert!(
            settings.validate().is_err(),
            "overwrite field should be allowed only with the 'MustRunAs' rule"
        );

        settings = Settings {
            run_as_user: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            run_as_group: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![],
                ..Default::default()
            },
            supplemental_groups: RuleStrategy {
                rule: String::from("RunAsAny"),
                ranges: vec![IDRange {
                    min: 1000,
                    max: 1010,
                }],
                overwrite: true,
            },
        };
        assert!(
            settings.validate().is_err(),
            "overwrite field should be allowed only with the 'MustRunAs' rule"
        );

        Ok(())
    }
}
