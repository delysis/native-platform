use crate::ContractError;
use crate::error::{ErrorClass, ServiceErrorV0};
use crate::ids::{AttemptId, OperationId};
use serde::{Deserialize, Serialize};

pub const TERMINAL_SCHEMA_V0: &str = "delysis.operation_terminal.v0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Reserved,
    Queued,
    Running,
    Terminal,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalClass {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalV0 {
    pub schema: String,
    pub operation_id: OperationId,
    pub attempt_id: Option<AttemptId>,
    pub class: TerminalClass,
    pub error: Option<ServiceErrorV0>,
}

impl TerminalV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != TERMINAL_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        match (&self.class, &self.error) {
            (TerminalClass::Completed, None) => Ok(()),
            (TerminalClass::Cancelled, Some(error)) if error.class == ErrorClass::Cancelled => {
                self.validate_error(error)
            }
            (TerminalClass::Failed, Some(error)) if error.class != ErrorClass::Cancelled => {
                self.validate_error(error)
            }
            _ => Err(ContractError::Inconsistent { field: "error" }),
        }
    }

    fn validate_error(&self, error: &ServiceErrorV0) -> Result<(), ContractError> {
        error.validate()?;
        if error.operation_id.as_ref() != Some(&self.operation_id) {
            return Err(ContractError::Inconsistent {
                field: "error.operation_id",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RetryAdvice, SERVICE_ERROR_SCHEMA_V0};
    use crate::ids::ServiceId;

    fn error(class: ErrorClass) -> ServiceErrorV0 {
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: if class == ErrorClass::Cancelled {
                "cancelled"
            } else {
                "worker.failed"
            }
            .to_owned(),
            class,
            retry: RetryAdvice::Never,
            operation_id: Some(OperationId::new("op-1").expect("operation ID")),
            service: ServiceId::new("test").expect("service ID"),
            safe_detail: "safe".to_owned(),
        }
    }

    fn terminal(class: TerminalClass, error: Option<ServiceErrorV0>) -> TerminalV0 {
        TerminalV0 {
            schema: TERMINAL_SCHEMA_V0.to_owned(),
            operation_id: OperationId::new("op-1").expect("operation ID"),
            attempt_id: None,
            class,
            error,
        }
    }

    #[test]
    fn terminal_class_and_error_must_agree() {
        assert!(terminal(TerminalClass::Completed, None).validate().is_ok());
        assert!(
            terminal(TerminalClass::Cancelled, Some(error(ErrorClass::Cancelled)))
                .validate()
                .is_ok()
        );
        assert!(
            terminal(TerminalClass::Failed, Some(error(ErrorClass::Worker)))
                .validate()
                .is_ok()
        );
        assert!(
            terminal(TerminalClass::Completed, Some(error(ErrorClass::Worker)))
                .validate()
                .is_err()
        );
        assert!(terminal(TerminalClass::Cancelled, None).validate().is_err());
        assert!(
            terminal(TerminalClass::Failed, Some(error(ErrorClass::Cancelled)))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn terminal_error_must_identify_the_enclosing_operation() {
        for class in [TerminalClass::Cancelled, TerminalClass::Failed] {
            let error_class = if class == TerminalClass::Cancelled {
                ErrorClass::Cancelled
            } else {
                ErrorClass::Worker
            };

            let mut missing = error(error_class);
            missing.operation_id = None;
            assert_eq!(
                terminal(class, Some(missing)).validate(),
                Err(ContractError::Inconsistent {
                    field: "error.operation_id"
                })
            );

            let mut mismatched = error(error_class);
            mismatched.operation_id = Some(OperationId::new("op-2").expect("operation ID"));
            assert_eq!(
                terminal(class, Some(mismatched)).validate(),
                Err(ContractError::Inconsistent {
                    field: "error.operation_id"
                })
            );
        }
    }

    #[test]
    fn unknown_terminal_field_is_rejected() {
        let mut json = serde_json::to_value(terminal(TerminalClass::Completed, None))
            .expect("serialize terminal");
        json.as_object_mut()
            .expect("terminal object")
            .insert("authority".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<TerminalV0>(json).is_err());
    }
}
