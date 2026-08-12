use crate::ids::{OperationId, ServiceId};
use crate::{ContractError, validate_nonempty_bounded};
use serde::{Deserialize, Serialize};

pub const SERVICE_ERROR_SCHEMA_V0: &str = "delysis.service_error.v0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    InvalidRequest,
    Unsupported,
    Unavailable,
    Permission,
    Privacy,
    Cancelled,
    Timeout,
    ResourceExhausted,
    Integrity,
    Publication,
    Storage,
    Worker,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryAdvice {
    Immediate,
    AfterUserAction,
    AfterRestart,
    DifferentRoute,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceErrorV0 {
    pub schema: String,
    pub code: String,
    pub class: ErrorClass,
    pub retry: RetryAdvice,
    pub operation_id: Option<OperationId>,
    pub service: ServiceId,
    pub safe_detail: String,
}

impl ServiceErrorV0 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != SERVICE_ERROR_SCHEMA_V0 {
            return Err(ContractError::Invalid { field: "schema" });
        }
        validate_code(&self.code)?;
        validate_nonempty_bounded("safe_detail", &self.safe_detail, 2048)
    }
}

fn validate_code(code: &str) -> Result<(), ContractError> {
    validate_nonempty_bounded("code", code, 128)?;
    if code.split('.').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }) {
        return Err(ContractError::Invalid { field: "code" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ServiceErrorV0 {
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: "timeout.waiter".to_owned(),
            class: ErrorClass::Timeout,
            retry: RetryAdvice::Immediate,
            operation_id: Some(OperationId::new("op-1").expect("valid operation ID")),
            service: ServiceId::new("gateway").expect("valid service ID"),
            safe_detail: "the caller stopped waiting".to_owned(),
        }
    }

    #[test]
    fn stable_namespaced_error_validates() {
        assert_eq!(fixture().validate(), Ok(()));
    }

    #[test]
    fn free_form_error_code_is_rejected() {
        let mut error = fixture();
        error.code = "Try Again!".to_owned();
        assert_eq!(
            error.validate(),
            Err(ContractError::Invalid { field: "code" })
        );
    }

    #[test]
    fn internal_error_source_is_not_serialized() {
        let json = serde_json::to_value(fixture()).expect("serialize fixture");
        assert!(json.get("source").is_none());
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let mut json = serde_json::to_value(fixture()).expect("serialize fixture");
        json.as_object_mut()
            .expect("fixture is an object")
            .insert("source".to_owned(), serde_json::json!("secret"));
        assert!(serde_json::from_value::<ServiceErrorV0>(json).is_err());
    }
}
