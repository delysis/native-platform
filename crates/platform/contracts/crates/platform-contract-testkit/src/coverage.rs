//! Acceptance accounting for composable lifecycle suites.
//!
//! Evidence can only be minted by this crate after a suite returns. A manifest
//! accepts evidence from several product components, but only for one product
//! and only when the union covers every normative invariant.

use std::collections::BTreeSet;

const MAX_COVERAGE_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum LifecycleInvariant {
    ExactTransitionChain,
    AttemptIdentityUnderPublicOperation,
    DuplicatePublicId,
    StaleRelease,
    TicketDropCancellation,
    ExplicitConsumerDropCancellation,
    AuthoritativeTerminal,
    CancelCompleteRace,
    AdmitQuiesceRace,
    QuiesceWaitsForReleaseAndJoin,
    WaiterTimeoutIsObservational,
    UnreadProgressCannotBlockFinalOrShutdown,
    ProgressTerminalRace,
    PanicTerminalAndShutdown,
    RepeatedShutdown,
    ShutdownEmpty,
    RetainedTasksBoundedByActiveConcurrency,
    SequenceExhaustion,
}

pub const REQUIRED_LIFECYCLE_INVARIANTS: [LifecycleInvariant; 18] = [
    LifecycleInvariant::ExactTransitionChain,
    LifecycleInvariant::AttemptIdentityUnderPublicOperation,
    LifecycleInvariant::DuplicatePublicId,
    LifecycleInvariant::StaleRelease,
    LifecycleInvariant::TicketDropCancellation,
    LifecycleInvariant::ExplicitConsumerDropCancellation,
    LifecycleInvariant::AuthoritativeTerminal,
    LifecycleInvariant::CancelCompleteRace,
    LifecycleInvariant::AdmitQuiesceRace,
    LifecycleInvariant::QuiesceWaitsForReleaseAndJoin,
    LifecycleInvariant::WaiterTimeoutIsObservational,
    LifecycleInvariant::UnreadProgressCannotBlockFinalOrShutdown,
    LifecycleInvariant::ProgressTerminalRace,
    LifecycleInvariant::PanicTerminalAndShutdown,
    LifecycleInvariant::RepeatedShutdown,
    LifecycleInvariant::ShutdownEmpty,
    LifecycleInvariant::RetainedTasksBoundedByActiveConcurrency,
    LifecycleInvariant::SequenceExhaustion,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageBinding {
    product: String,
    implementation: String,
    component: String,
}

impl CoverageBinding {
    pub fn new(
        product: &str,
        implementation: &str,
        component: &str,
    ) -> Result<Self, AcceptanceError> {
        validate_id("product", product)?;
        validate_id("implementation", implementation)?;
        validate_id("component", component)?;
        Ok(Self {
            product: product.to_owned(),
            implementation: implementation.to_owned(),
            component: component.to_owned(),
        })
    }

    pub fn product(&self) -> &str {
        &self.product
    }

    pub fn component(&self) -> &str {
        &self.component
    }

    pub fn implementation(&self) -> &str {
        &self.implementation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageEvidence {
    binding: CoverageBinding,
    suite: &'static str,
    invariants: BTreeSet<LifecycleInvariant>,
}

impl CoverageEvidence {
    pub(crate) fn passed(
        binding: &CoverageBinding,
        suite: &'static str,
        invariants: impl IntoIterator<Item = LifecycleInvariant>,
    ) -> Self {
        Self {
            binding: binding.clone(),
            suite,
            invariants: invariants.into_iter().collect(),
        }
    }

    pub fn binding(&self) -> &CoverageBinding {
        &self.binding
    }

    pub const fn suite(&self) -> &'static str {
        self.suite
    }

    pub fn invariants(&self) -> impl Iterator<Item = LifecycleInvariant> + '_ {
        self.invariants.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleCoverageManifest {
    product: String,
    implementation: String,
    components: BTreeSet<String>,
    covered: BTreeSet<LifecycleInvariant>,
}

impl LifecycleCoverageManifest {
    pub fn accept(
        product: &str,
        implementation: &str,
        evidence: impl IntoIterator<Item = CoverageEvidence>,
    ) -> Result<Self, AcceptanceError> {
        validate_id("product", product)?;
        validate_id("implementation", implementation)?;
        let evidence = evidence.into_iter().collect::<Vec<_>>();
        if evidence.is_empty() {
            return Err(AcceptanceError::NoEvidence);
        }

        let mut components = BTreeSet::new();
        let mut covered = BTreeSet::new();
        for item in evidence {
            if item.binding.product != product {
                return Err(AcceptanceError::WrongProduct {
                    expected: product.to_owned(),
                    actual: item.binding.product,
                });
            }
            if item.binding.implementation != implementation {
                return Err(AcceptanceError::WrongImplementation {
                    expected: implementation.to_owned(),
                    actual: item.binding.implementation,
                });
            }
            components.insert(item.binding.component);
            covered.extend(item.invariants);
        }
        let missing = REQUIRED_LIFECYCLE_INVARIANTS
            .iter()
            .copied()
            .filter(|invariant| !covered.contains(invariant))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(AcceptanceError::MissingInvariants(missing));
        }
        Ok(Self {
            product: product.to_owned(),
            implementation: implementation.to_owned(),
            components,
            covered,
        })
    }

    pub fn product(&self) -> &str {
        &self.product
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.components.iter().map(String::as_str)
    }

    pub fn implementation(&self) -> &str {
        &self.implementation
    }

    pub fn covered(&self) -> impl Iterator<Item = LifecycleInvariant> + '_ {
        self.covered.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AcceptanceError {
    #[error(
        "{field} coverage identity must be nonempty bounded safe printable ASCII without quotes or backslashes"
    )]
    InvalidIdentity { field: &'static str },
    #[error("lifecycle acceptance received no suite evidence")]
    NoEvidence,
    #[error("coverage evidence names product {actual}, expected {expected}")]
    WrongProduct { expected: String, actual: String },
    #[error("coverage evidence names implementation {actual}, expected {expected}")]
    WrongImplementation { expected: String, actual: String },
    #[error("lifecycle coverage is missing required invariants: {0:?}")]
    MissingInvariants(Vec<LifecycleInvariant>),
}

fn validate_id(field: &'static str, value: &str) -> Result<(), AcceptanceError> {
    if value.is_empty()
        || value.len() > MAX_COVERAGE_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return Err(AcceptanceError::InvalidIdentity { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_bindings_reject_ambiguous_or_unsafe_identity() {
        assert!(CoverageBinding::new("loom", "interactive", "host").is_ok());
        assert!(matches!(
            CoverageBinding::new("loom", "interactive generation", "host"),
            Err(AcceptanceError::InvalidIdentity {
                field: "implementation"
            })
        ));
        assert!(CoverageBinding::new("loom", "interactive", "bad\\component").is_err());
    }
}
