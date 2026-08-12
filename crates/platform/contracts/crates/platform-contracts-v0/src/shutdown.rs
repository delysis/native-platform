use crate::ContractError;
use crate::error::ServiceErrorV0;
use crate::ids::ServiceId;
use crate::lifecycle::SupervisorPhase;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CLOSED_SUMMARY_SCHEMA_V0: &str = "delysis.closed_summary.v0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownFailureV0 {
    pub service: ServiceId,
    pub error: ServiceErrorV0,
}

impl ShutdownFailureV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.error.validate()?;
        if self.error.service != self.service {
            return Err(ContractError::Inconsistent { field: "service" });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedSummaryV0 {
    pub schema: String,
    pub phase: SupervisorPhase,
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub joined_workers: usize,
    pub failures: Vec<ShutdownFailureV0>,
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
        let mut services = BTreeSet::new();
        for failure in &self.failures {
            failure.validate()?;
            if !services.insert(&failure.service) {
                return Err(ContractError::Duplicate {
                    field: "failures.service",
                });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_summary_requires_no_live_or_retained_work() {
        let mut summary = ClosedSummaryV0 {
            schema: CLOSED_SUMMARY_SCHEMA_V0.to_owned(),
            phase: SupervisorPhase::Closed,
            active_operations: 0,
            retained_tasks: 0,
            joined_workers: 1,
            failures: Vec::new(),
        };
        assert!(summary.validate().is_ok());
        assert!(summary.succeeded());
        summary.retained_tasks = 1;
        assert!(summary.validate().is_err());
    }

    #[test]
    fn unknown_summary_fields_fail_closed() {
        let json = serde_json::json!({
            "schema": CLOSED_SUMMARY_SCHEMA_V0,
            "phase": "closed",
            "active_operations": 0,
            "retained_tasks": 0,
            "joined_workers": 0,
            "failures": [],
            "detached_tasks": 1
        });
        assert!(serde_json::from_value::<ClosedSummaryV0>(json).is_err());
    }
}
