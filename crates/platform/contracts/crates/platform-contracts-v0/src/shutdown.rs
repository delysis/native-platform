use crate::error::ServiceErrorV0;
use crate::ids::ServiceId;
use crate::lifecycle::SupervisorPhase;
use crate::{ContractError, validate_nonempty_bounded};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CLOSED_SUMMARY_SCHEMA_V0: &str = "delysis.closed_summary.v0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownResourceKind {
    OperationRegistry,
    TaskSupervisor,
    Backend,
    WorkerPool,
    Runtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownResourceState {
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownResourceV0 {
    /// Opaque stable identity; never a filesystem path or authority token.
    pub resource_id: String,
    pub service: ServiceId,
    pub kind: ShutdownResourceKind,
    pub state: ShutdownResourceState,
    pub expected_workers: usize,
    pub joined_workers: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShutdownResourceV0 {
    resource_id: String,
    service: ServiceId,
    kind: ShutdownResourceKind,
    state: ShutdownResourceState,
    expected_workers: usize,
    joined_workers: usize,
}

impl<'de> Deserialize<'de> for ShutdownResourceV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawShutdownResourceV0::deserialize(deserializer)?;
        let value = Self {
            resource_id: raw.resource_id,
            service: raw.service,
            kind: raw.kind,
            state: raw.state,
            expected_workers: raw.expected_workers,
            joined_workers: raw.joined_workers,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl ShutdownResourceV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_opaque_id("resource_id", &self.resource_id)?;
        if self.joined_workers > self.expected_workers {
            return Err(ContractError::Inconsistent {
                field: "resource.joined_workers",
            });
        }
        if self.state == ShutdownResourceState::Stopped
            && self.joined_workers != self.expected_workers
        {
            return Err(ContractError::Inconsistent {
                field: "resource.state",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownFailureV0 {
    pub failure_id: String,
    pub resource_id: String,
    pub service: ServiceId,
    pub error: ServiceErrorV0,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShutdownFailureV0 {
    failure_id: String,
    resource_id: String,
    service: ServiceId,
    error: ServiceErrorV0,
}

impl<'de> Deserialize<'de> for ShutdownFailureV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawShutdownFailureV0::deserialize(deserializer)?;
        let value = Self {
            failure_id: raw.failure_id,
            resource_id: raw.resource_id,
            service: raw.service,
            error: raw.error,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl ShutdownFailureV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_opaque_id("failure_id", &self.failure_id)?;
        validate_opaque_id("failure.resource_id", &self.resource_id)?;
        self.error.validate()?;
        if self.error.service != self.service {
            return Err(ContractError::Inconsistent {
                field: "failure.service",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedSummaryV0 {
    pub schema: String,
    pub phase: SupervisorPhase,
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_workers: usize,
    pub joined_workers: usize,
    pub resources: Vec<ShutdownResourceV0>,
    pub failures: Vec<ShutdownFailureV0>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClosedSummaryV0 {
    schema: String,
    phase: SupervisorPhase,
    active_operations: usize,
    retained_tasks: usize,
    expected_workers: usize,
    joined_workers: usize,
    resources: Vec<ShutdownResourceV0>,
    failures: Vec<ShutdownFailureV0>,
}

impl<'de> Deserialize<'de> for ClosedSummaryV0 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawClosedSummaryV0::deserialize(deserializer)?;
        let value = Self {
            schema: raw.schema,
            phase: raw.phase,
            active_operations: raw.active_operations,
            retained_tasks: raw.retained_tasks,
            expected_workers: raw.expected_workers,
            joined_workers: raw.joined_workers,
            resources: raw.resources,
            failures: raw.failures,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl ClosedSummaryV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != CLOSED_SUMMARY_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        if self.phase != SupervisorPhase::Closed {
            return Err(ContractError::Invalid { field: "phase" });
        }
        if self.active_operations != 0 {
            return Err(ContractError::Invalid {
                field: "active_operations",
            });
        }
        if self.retained_tasks != 0 {
            return Err(ContractError::Invalid {
                field: "retained_tasks",
            });
        }
        if self.resources.is_empty() {
            return Err(ContractError::Empty { field: "resources" });
        }

        let mut resources = BTreeMap::new();
        let mut expected_workers = 0usize;
        let mut joined_workers = 0usize;
        for resource in &self.resources {
            resource.validate()?;
            if resources.insert(&resource.resource_id, resource).is_some() {
                return Err(ContractError::Duplicate {
                    field: "resources.resource_id",
                });
            }
            expected_workers = expected_workers
                .checked_add(resource.expected_workers)
                .ok_or(ContractError::Invalid {
                    field: "expected_workers",
                })?;
            joined_workers = joined_workers.checked_add(resource.joined_workers).ok_or(
                ContractError::Invalid {
                    field: "joined_workers",
                },
            )?;
        }
        if self.expected_workers != expected_workers {
            return Err(ContractError::Inconsistent {
                field: "expected_workers",
            });
        }
        if self.joined_workers != joined_workers {
            return Err(ContractError::Inconsistent {
                field: "joined_workers",
            });
        }

        let mut failure_ids = BTreeSet::new();
        let mut failed_resources = BTreeSet::new();
        for failure in &self.failures {
            failure.validate()?;
            if !failure_ids.insert(&failure.failure_id) {
                return Err(ContractError::Duplicate {
                    field: "failures.failure_id",
                });
            }
            let resource =
                resources
                    .get(&failure.resource_id)
                    .ok_or(ContractError::Inconsistent {
                        field: "failure.resource_id",
                    })?;
            if resource.service != failure.service {
                return Err(ContractError::Inconsistent {
                    field: "failure.service",
                });
            }
            failed_resources.insert(failure.resource_id.as_str());
        }

        for resource in &self.resources {
            let has_failure = failed_resources.contains(resource.resource_id.as_str());
            match (resource.state, has_failure) {
                (ShutdownResourceState::Stopped, false) | (ShutdownResourceState::Failed, true) => {
                }
                _ => {
                    return Err(ContractError::Inconsistent {
                        field: "resource.state",
                    });
                }
            }
        }

        if self.failures.is_empty()
            && (self.joined_workers != self.expected_workers
                || self
                    .resources
                    .iter()
                    .any(|resource| resource.state != ShutdownResourceState::Stopped))
        {
            return Err(ContractError::Inconsistent {
                field: "successful.resources",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

fn validate_opaque_id(field: &'static str, value: &str) -> Result<(), ContractError> {
    validate_nonempty_bounded(field, value, 256)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ContractError::Invalid { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorClass, RetryAdvice, SERVICE_ERROR_SCHEMA_V0};

    fn resource(
        resource_id: &str,
        service: &str,
        state: ShutdownResourceState,
        expected_workers: usize,
        joined_workers: usize,
    ) -> ShutdownResourceV0 {
        ShutdownResourceV0 {
            resource_id: resource_id.to_owned(),
            service: ServiceId::new(service).expect("service ID"),
            kind: ShutdownResourceKind::TaskSupervisor,
            state,
            expected_workers,
            joined_workers,
        }
    }

    fn failure(failure_id: &str, resource_id: &str, service: &str) -> ShutdownFailureV0 {
        ShutdownFailureV0 {
            failure_id: failure_id.to_owned(),
            resource_id: resource_id.to_owned(),
            service: ServiceId::new(service).expect("service ID"),
            error: ServiceErrorV0 {
                schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
                code: "shutdown.worker_join".to_owned(),
                class: ErrorClass::Worker,
                retry: RetryAdvice::AfterRestart,
                operation_id: None,
                service: ServiceId::new(service).expect("service ID"),
                safe_detail: "worker join failed".to_owned(),
            },
        }
    }

    fn successful_summary() -> ClosedSummaryV0 {
        ClosedSummaryV0 {
            schema: CLOSED_SUMMARY_SCHEMA_V0.to_owned(),
            phase: SupervisorPhase::Closed,
            active_operations: 0,
            retained_tasks: 0,
            expected_workers: 2,
            joined_workers: 2,
            resources: vec![resource(
                "speech.host.tasks",
                "speech-host",
                ShutdownResourceState::Stopped,
                2,
                2,
            )],
            failures: Vec::new(),
        }
    }

    #[test]
    fn successful_closed_summary_requires_exact_resource_accounting() {
        let mut summary = successful_summary();
        assert!(summary.validate().is_ok());
        assert!(summary.succeeded());

        summary.joined_workers = 1;
        assert!(summary.validate().is_err());
        summary.joined_workers = 2;
        summary.resources[0].joined_workers = 1;
        assert!(summary.validate().is_err());
        summary.resources[0].joined_workers = 2;
        summary.retained_tasks = 1;
        assert!(summary.validate().is_err());
    }

    #[test]
    fn multiple_failures_may_share_a_service_and_resource() {
        let mut summary = successful_summary();
        summary.resources[0].state = ShutdownResourceState::Failed;
        summary.resources[0].joined_workers = 1;
        summary.joined_workers = 1;
        summary.failures = vec![
            failure("speech.join.1", "speech.host.tasks", "speech-host"),
            failure("speech.join.2", "speech.host.tasks", "speech-host"),
        ];
        assert!(summary.validate().is_ok());

        summary.failures[1].failure_id = "speech.join.1".to_owned();
        assert_eq!(
            summary.validate(),
            Err(ContractError::Duplicate {
                field: "failures.failure_id"
            })
        );
    }

    #[test]
    fn failure_must_match_a_declared_resource_and_service() {
        let mut summary = successful_summary();
        summary.resources[0].state = ShutdownResourceState::Failed;
        summary.failures = vec![failure("speech.join.1", "speech.missing", "speech-host")];
        assert!(summary.validate().is_err());

        summary.failures[0].resource_id = "speech.host.tasks".to_owned();
        summary.failures[0].service = ServiceId::new("other-service").expect("service ID");
        summary.failures[0].error.service = summary.failures[0].service.clone();
        assert!(summary.validate().is_err());
    }

    #[test]
    fn unknown_summary_fields_fail_closed() {
        let mut json = serde_json::to_value(successful_summary()).expect("summary JSON");
        json.as_object_mut()
            .expect("summary object")
            .insert("authority".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<ClosedSummaryV0>(json).is_err());
    }
}
