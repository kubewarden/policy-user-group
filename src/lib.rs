use lazy_static::lazy_static;

extern crate wapc_guest as guest;
use guest::prelude::*;

use anyhow::{anyhow, Result};
use k8s_openapi::api::core::v1 as apicore;

extern crate kubewarden_policy_sdk as kubewarden;
use kubewarden::{logging, protocol_version_guest, request::ValidationRequest, validate_settings};

mod settings;
use settings::Settings;

use slog::{info, o, Logger};

lazy_static! {
    static ref LOG_DRAIN: Logger = Logger::root(
        logging::KubewardenDrain::new(),
        o!("policy" => "sample-policy")
    );
}
const MUST_RUN_AS: &str = "MustRunAs";
const MUST_RUN_AS_NON_ROOT: &str = "MustRunAsNonRoot";
const MAY_RUN_AS: &str = "MayRunAs";

#[no_mangle]
pub extern "C" fn wapc_init() {
    register_function("validate", validate);
    register_function("validate_settings", validate_settings::<Settings>);
    register_function("protocol_version", protocol_version_guest);
}

fn enforce_run_as_user_rule(
    security_context_option: Option<apicore::SecurityContext>,
    validation_request: &ValidationRequest<Settings>,
) -> Result<Option<apicore::SecurityContext>> {
    let mut security_context = security_context_option.unwrap_or_default();
    match validation_request.settings.run_as_user.rule.as_str() {
        MUST_RUN_AS => {
            if validation_request.settings.run_as_user.overwrite
                || security_context.run_as_user.is_none()
            {
                let default_user_id = validation_request.settings.run_as_user.ranges[0].min;
                security_context.run_as_user = Some(default_user_id);
                return Ok(Some(security_context));
            }
            if let Some(user_id) = security_context.run_as_user {
                if !validation_request.settings.run_as_user.is_valid_id(user_id) {
                    return Err(anyhow!("User ID outside defined ranges"));
                }
            }
        }
        MUST_RUN_AS_NON_ROOT => {
            if let Some(run_as_non_root) = security_context.run_as_non_root {
                if !run_as_non_root {
                    return Err(anyhow!("RunAsNonRoot should be set to true"));
                }
            }
            if let Some(user_id) = security_context.run_as_user {
                if user_id == 0 {
                    return Err(anyhow!(
                        "Invalid user ID: cannot run container with root ID (0)"
                    ));
                }
            }
            security_context.run_as_non_root = Some(true);
            return Ok(Some(security_context));
        }
        &_ => {}
    }
    Ok(None)
}

fn enforce_run_as_group(
    security_context_option: Option<apicore::SecurityContext>,
    validation_request: &ValidationRequest<Settings>,
) -> Result<Option<apicore::SecurityContext>> {
    let mut security_context = security_context_option.unwrap_or_default();
    match validation_request.settings.run_as_group.rule.as_str() {
        MUST_RUN_AS => {
            if validation_request.settings.run_as_group.overwrite
                || security_context.run_as_group.is_none()
            {
                let default_group_id = validation_request.settings.run_as_group.ranges[0].min;
                security_context.run_as_group = Some(default_group_id);
                return Ok(Some(security_context));
            }
            if let Some(group_id) = security_context.run_as_group {
                if !validation_request
                    .settings
                    .run_as_group
                    .is_valid_id(group_id)
                {
                    return Err(anyhow!("Group ID is outside defined ranges"));
                }
            }
        }
        MAY_RUN_AS => {
            if let Some(group_id) = security_context.run_as_group {
                if !validation_request
                    .settings
                    .run_as_group
                    .is_valid_id(group_id)
                {
                    return Err(anyhow!("Group ID is outside defined ranges"));
                }
            }
        }
        &_ => {}
    }
    Ok(None)
}

fn enforce_supplemental_groups(
    security_context_option: Option<apicore::PodSecurityContext>,
    validation_request: &ValidationRequest<Settings>,
) -> Result<Option<apicore::PodSecurityContext>> {
    let mut security_context = security_context_option.unwrap_or_default();
    match validation_request
        .settings
        .supplemental_groups
        .rule
        .as_str()
    {
        MUST_RUN_AS => {
            if validation_request.settings.supplemental_groups.overwrite
                || security_context.supplemental_groups.is_none()
            {
                let default_group_id =
                    validation_request.settings.supplemental_groups.ranges[0].min;
                security_context.supplemental_groups = Some(vec![default_group_id]);
                return Ok(Some(security_context));
            }
            if let Some(group_ids) = security_context.supplemental_groups {
                for group_id in group_ids {
                    if !validation_request
                        .settings
                        .supplemental_groups
                        .is_valid_id(group_id)
                    {
                        return Err(anyhow!("Group ID is outside defined ranges"));
                    }
                }
            }
        }
        MAY_RUN_AS => {
            if let Some(group_ids) = security_context.supplemental_groups {
                for group_id in group_ids {
                    if !validation_request
                        .settings
                        .supplemental_groups
                        .is_valid_id(group_id)
                    {
                        return Err(anyhow!("Group ID is outside defined ranges"));
                    }
                }
            }
        }
        &_ => (),
    }
    Ok(None)
}

fn enforce_container_security_policies(
    container: &mut apicore::Container,
    validation_request: &ValidationRequest<Settings>,
) -> Result<bool> {
    let mut mutated: bool = false;
    let mutate_request =
        enforce_run_as_user_rule(container.security_context.clone(), validation_request)?;
    if mutate_request.is_some() {
        mutated = true;
        container.security_context = mutate_request;
    }
    let mutate_request =
        enforce_run_as_group(container.security_context.clone(), validation_request)?;
    if mutate_request.is_some() {
        mutated = true;
        container.security_context = mutate_request;
    }
    Ok(mutated)
}

fn validate(payload: &[u8]) -> CallResult {
    info!(LOG_DRAIN, "starting validation");
    let validation_request: ValidationRequest<Settings> = ValidationRequest::new(payload)?;
    let mut pod: apicore::Pod =
        serde_json::from_value(validation_request.request.object.clone())
            .map_err(|e| anyhow!("Error deserializing Pod specification: {:?}", e))?;
    let mut mutated: bool = false;

    if pod.spec.is_none() {
        return kubewarden::accept_request();
    }
    let mut pod_spec = pod.spec.unwrap();

    if let Some(init_containers) = pod_spec.init_containers.as_mut() {
        for init_container in init_containers.iter_mut() {
            match enforce_container_security_policies(init_container, &validation_request) {
                Ok(mutate_request) => mutated = mutated || mutate_request,
                Err(error) => return kubewarden::reject_request(Some(error.to_string()), None),
            }
        }
    }
    for container in pod_spec.containers.iter_mut() {
        match enforce_container_security_policies(container, &validation_request) {
            Ok(mutate_request) => mutated = mutated || mutate_request,
            Err(error) => return kubewarden::reject_request(Some(error.to_string()), None),
        }
    }

    match enforce_supplemental_groups(pod_spec.security_context.clone(), &validation_request) {
        Ok(mutate_security_context) => {
            if mutate_security_context.is_some() {
                mutated = true;
                pod_spec.security_context = mutate_security_context;
            }
        }
        Err(msg) => {
            return kubewarden::reject_request(Some(msg.to_string()), None);
        }
    }

    pod.spec = Some(pod_spec);
    if mutated {
        kubewarden::mutate_request(serde_json::to_value(pod)?)
    } else {
        kubewarden::accept_request()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use jsonpath_lib as jsonpath;
    use kubewarden_policy_sdk::test::Testcase;
    use settings::{IDRange, RuleStrategy};

    #[test]
    fn may_run_as_should_accept_request_if_supplemental_group_id_is_valid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_valid_supplemental_group.json";
        let tc = Testcase {
            name: String::from(
                "MayRunAs should accept request when valid SupplementalGroups value is set",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                        max: 4000,
                    }],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_reject_request_if_supplemental_group_is_invalid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_invalid_supplemental_group.json";
        let tc = Testcase {
            name: String::from(
                "MayRunAs should reject request when no SupplementalGroups value is invalid",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
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
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_accept_request_if_supplemental_group_is_not_defined() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_value.json";
        let tc = Testcase {
            name: String::from(
                "MayRunAs should accept request when no SupplementalGroups value is set",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_mutate_request_when_supplemental_group_id_is_not_defined(
    ) -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_value.json";
        let tc = Testcase {
            name: String::from(
                "'MustRunAs' should mutate request when supplemental group ID is missing",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request"
        );
        let supplemental_groups_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.securityContext.supplementalGroups[*]",
        )
        .unwrap();
        assert_eq!(
            supplemental_groups_json,
            vec![1500],
            "MustRunAs should add the 'supplementalGroups' when it is not defined"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_accept_when_supplemental_group_id_is_valid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_valid_supplemental_group.json";
        let tc = Testcase {
            name: String::from(
                "'MustRunAs' should accept request when supplemental group ID is valid",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_reject_when_supplemental_group_id_is_invalid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_invalid_supplemental_group.json";
        let tc = Testcase {
            name: String::from(
                "'MustRunAs' should reject request when supplemental group ID is invalid",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
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
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_non_root_should_reject_request_when_run_as_non_root_is_false() -> Result<(), ()>
    {
        let request_file = "test_data/pod_creation_run_as_non_root_is_false.json";
        let tc = Testcase {
            name: String::from("MustRunAsNonRoot should reject request if 'runAsNonRoot' is false"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAsNonRoot"),
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
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_non_root_should_mutate_request_when_run_as_user_is_not_defined() -> Result<(), ()>
    {
        let request_file = "test_data/pod_creation_must_run_as_with_no_user_id.json";
        let tc = Testcase {
            name: String::from(
                "MustRunAsNonRoot should add 'runAsNonRoot' when 'runAsUser' is not defined",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAsNonRoot"),
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
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(res.mutated_object.is_some(), "Request should be mutated");
        let run_as_non_root_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsNonRoot",
        )
        .unwrap();
        assert_eq!(
            run_as_non_root_json,
            vec![true],
            "MustRunAsNonRoot should add the 'runAsNonRoot' when no 'runAsUser' is not defined"
        );
        let run_as_non_root_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsNonRoot",
        )
        .unwrap();
        assert_eq!(
            run_as_non_root_json,
            vec![true],
            "MustRunAsNonRoot should add the 'runAsNonRoot' when no 'runAsUser' is not defined"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_non_root_should_reject_zero_as_user_id() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_zero_user_id.json";
        let tc = Testcase {
            name: String::from("MustRunAsNonRoot rule does not allow 0 as user ID"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAsNonRoot"),
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
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_accept_request_if_group_id_is_valid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_group_id.json";
        let tc = Testcase {
            name: String::from("MayRunAs should accept request when valid RunAsGroup value is set"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MayRunAs"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_reject_request_if_run_as_group_value_is_invalid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_invalid_value.json";
        let tc = Testcase {
            name: String::from("MayRunAs should mutate object when no RunAsGroup value is invalid"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MayRunAs"),
                    ranges: vec![IDRange {
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_accecpt_request_if_run_as_group_is_not_defined() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_value.json";
        let tc = Testcase {
            name: String::from("MayRunAs should not mutate object when no RunAsGroup value is set"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MayRunAs"),
                    ranges: vec![IDRange {
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_rule_should_mutate_pod_when_no_values_id() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_value.json";
        let tc = Testcase {
            name: String::from("MustRunAs should mutate object when invalid values are set"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 1500,
                        max: 2000,
                    }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(res.mutated_object.is_some(), "Request should be mutated");
        let user_id_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            user_id_json,
            vec![1500],
            "MustRunAs should mutate object when invalid user ID is set"
        );

        let group_id_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            group_id_json,
            vec![2000],
            "MustRunAs should mutate object when invalid group ID is set"
        );
        let user_id_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            user_id_json,
            vec![1500],
            "MustRunAs should mutate object when invalid user ID is set"
        );

        let group_id_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            group_id_json,
            vec![2000],
            "MustRunAs should mutate object when invalid group ID is set"
        );

        Ok(())
    }

    #[test]
    fn must_run_as_rule_should_reject_request_when_invaid_values_id() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_invalid_value.json";
        let tc = Testcase {
            name: String::from("MustRunAs should mutate object when invalid values are set"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 1500,
                        max: 2000,
                    }],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 2000,
                        max: 2500,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Request should not be mutated"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_reject_when_group_id_is_invalid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_invalid_group_id.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should reject request when group ID is invalid"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request when no group ID is invalid"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_reject_when_user_id_is_invalid() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_invalid_user_id.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should reject request when user ID is invalid"),
            fixture_file: String::from(request_file),
            expected_validation_result: false,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
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
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request when no user ID is invalid"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_mutate_when_no_group_id_is_defined_adding_first_range_min_value(
    ) -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_group_id.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should mutate when no group ID is defined"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request when no group ID is defined"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1500],
            "Mutated group ID is not the first range's min value"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1500],
            "Mutated group ID is not the first range's min value"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_mutate_when_no_user_id_is_defined_adding_first_range_min_value(
    ) -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_no_user_id.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should mutate when no user ID is defined"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    ..Default::default()
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
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request when no user ID is defined"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1500],
            "Mutated user ID is not the first range's min value"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1500],
            "Mutated user ID is not the first range's min value"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_accept_when_valid_group_id_is_defined() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_group_id.json";
        let tc = Testcase {
            name: String::from(
                "MustRunAs should not mutate request when valid group ID is defined",
            ),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 1500,
                        max: 2000,
                    }],
                    ..Default::default()
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request when valid group ID is defined"
        );
        assert!(
            res.accepted,
            "MustRunAs should accept request when valid group ID is defined"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_accept_when_valid_user_id_is_defined() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_user_id.json";
        let tc = Testcase {
            name: String::from("MustRunAs should not mutate request when valid user ID is defined"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 1500,
                        max: 2000,
                    }],
                    ..Default::default()
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
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MustRunAs should not mutate request when valid user ID is defined"
        );
        assert!(
            res.accepted,
            "MustRunAs should accept request when valid user ID is defined"
        );
        Ok(())
    }

    #[test]
    fn run_as_any_should_not_mutate_pod() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_run_as_any.json";
        let tc = Testcase {
            name: String::from("RunAsAny should not mutate object"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "RunAsAny should not mutate request"
        );

        Ok(())
    }

    #[test]
    fn must_run_as_should_mutate_supplemental_group_if_overwrite_is_set() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_valid_supplemental_group.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should mutate request when overwrite is 'true'"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                    ranges: vec![
                        IDRange {
                            min: 1500,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    overwrite: true,
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request"
        );
        let supplemental_groups_json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.securityContext.supplementalGroups[*]",
        )
        .unwrap();
        assert_eq!(
            supplemental_groups_json,
            vec![1500],
            "MustRunAs should change 'supplementalGroups' when 'overwrite' setting is true"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_not_mutate_supplemental_group_if_overwrite_is_set() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_valid_supplemental_group.json";
        let tc = Testcase {
            name: String::from("'MayRunAs' should not mutate request when overwrite is 'true'"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
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
                        max: 4000,
                    }],
                    overwrite: true,
                },
            },
        };
        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "Overwrite should not mutate request"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_mutates_groups_if_overwrite_is_set() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_group_id.json";
        let tc = Testcase {
            name: String::from("'MustRunAs' should mutate groups when overwrite is 'true'"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1000,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    overwrite: true,
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1000],
            "MustRunAs should change 'runAsGroup' when 'overwrite' setting is true"
        );

        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsGroup",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1000],
            "MustRunAs should change 'runAsGroup' when 'overwrite' setting is true"
        );
        Ok(())
    }

    #[test]
    fn may_run_as_should_not_mutates_group_if_overwrite_is_set() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_group_id.json";
        let tc = Testcase {
            name: String::from("'MayRunAs' should mutate request when overwrite is 'true'"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
                run_as_group: RuleStrategy {
                    rule: String::from("MayRunAs"),
                    ranges: vec![
                        IDRange {
                            min: 1000,
                            max: 2000,
                        },
                        IDRange {
                            min: 2500,
                            max: 3000,
                        },
                    ],
                    overwrite: true,
                },
                supplemental_groups: RuleStrategy {
                    rule: String::from("RunAsAny"),
                    ranges: vec![],
                    ..Default::default()
                },
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_none(),
            "MayRunAs should not mutate request"
        );
        Ok(())
    }

    #[test]
    fn must_run_as_should_mutate_user_id_when_overwrite_is_true() -> Result<(), ()> {
        let request_file = "test_data/pod_creation_must_run_as_with_user_id.json";
        let tc = Testcase {
            name: String::from("MustRunAs should mutate request 'overwrite' is set"),
            fixture_file: String::from(request_file),
            expected_validation_result: true,
            settings: Settings {
                run_as_user: RuleStrategy {
                    rule: String::from("MustRunAs"),
                    ranges: vec![IDRange {
                        min: 1000,
                        max: 2000,
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
            },
        };

        let res = tc.eval(validate).unwrap();
        assert!(
            res.mutated_object.is_some(),
            "MustRunAs should mutate request when 'overwrite' is set"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.containers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1000],
            "Mutated user ID should be the first range's min value"
        );
        let json = jsonpath::select(
            res.mutated_object.as_ref().unwrap(),
            "$.spec.initContainers[*].securityContext.runAsUser",
        )
        .unwrap();
        assert_eq!(
            json,
            vec![1000],
            "Mutated user ID should be the first range's min value"
        );
        Ok(())
    }
}
