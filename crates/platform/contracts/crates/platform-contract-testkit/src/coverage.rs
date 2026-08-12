//! Acceptance accounting for composable lifecycle suites.
//!
//! Evidence is typed by its lifecycle implementation and can only be minted by
//! this crate after a suite returns. A manifest therefore cannot accept
//! evidence from another product or implementation by relabeling strings.

use std::collections::BTreeSet;
use std::marker::PhantomData;

const MAX_COVERAGE_ID_BYTES: usize = 128;

/// Compile-time identity for one concrete product lifecycle implementation.
///
/// An adapter selects this identity as an associated type. The suite runners
/// derive evidence identity from that type; callers provide only a component
/// name.
pub trait LifecycleImplementation: 'static {
    const PRODUCT: &'static str;
    const IMPLEMENTATION: &'static str;

    fn validate() -> Result<(), AcceptanceError> {
        validate_id("product", Self::PRODUCT)?;
        validate_id("implementation", Self::IMPLEMENTATION)
    }
}

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

pub struct CoverageEvidence<I: LifecycleImplementation> {
    component: String,
    suite: &'static str,
    invariants: BTreeSet<LifecycleInvariant>,
    implementation: PhantomData<fn() -> I>,
}

impl<I: LifecycleImplementation> CoverageEvidence<I> {
    pub(crate) fn passed(
        component: &str,
        suite: &'static str,
        invariants: impl IntoIterator<Item = LifecycleInvariant>,
    ) -> Self {
        I::validate().expect("adapter lifecycle identity must be valid");
        validate_id("component", component).expect("coverage component identity must be valid");
        Self {
            component: component.to_owned(),
            suite,
            invariants: invariants.into_iter().collect(),
            implementation: PhantomData,
        }
    }

    pub fn product(&self) -> &'static str {
        I::PRODUCT
    }

    pub fn component(&self) -> &str {
        &self.component
    }

    pub fn implementation(&self) -> &'static str {
        I::IMPLEMENTATION
    }

    pub const fn suite(&self) -> &'static str {
        self.suite
    }

    pub fn invariants(&self) -> impl Iterator<Item = LifecycleInvariant> + '_ {
        self.invariants.iter().copied()
    }
}

/// A complete manifest for exactly one compile-time lifecycle identity.
///
/// Evidence for another identity cannot be supplied:
///
/// ```compile_fail
/// use platform_contract_testkit::{
///     LifecycleCoverageManifest, LifecycleImplementation, ReferenceAdapter,
///     compositional_lifecycle::run_transition_chain_suite,
/// };
///
/// struct ForeignLifecycle;
/// impl LifecycleImplementation for ForeignLifecycle {
///     const PRODUCT: &'static str = "fte";
///     const IMPLEMENTATION: &'static str = "gateway";
/// }
///
/// let reference = run_transition_chain_suite::<ReferenceAdapter>("state-machine");
/// let _ = LifecycleCoverageManifest::<ForeignLifecycle>::accept([reference]);
/// ```
pub struct LifecycleCoverageManifest<I: LifecycleImplementation> {
    components: BTreeSet<String>,
    covered: BTreeSet<LifecycleInvariant>,
    implementation: PhantomData<fn() -> I>,
}

impl<I: LifecycleImplementation> LifecycleCoverageManifest<I> {
    pub fn accept(
        evidence: impl IntoIterator<Item = CoverageEvidence<I>>,
    ) -> Result<Self, AcceptanceError> {
        I::validate()?;
        let evidence = evidence.into_iter().collect::<Vec<_>>();
        if evidence.is_empty() {
            return Err(AcceptanceError::NoEvidence);
        }

        let mut components = BTreeSet::new();
        let mut covered = BTreeSet::new();
        for item in evidence {
            components.insert(item.component);
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
            components,
            covered,
            implementation: PhantomData,
        })
    }

    pub fn product(&self) -> &'static str {
        I::PRODUCT
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.components.iter().map(String::as_str)
    }

    pub fn implementation(&self) -> &'static str {
        I::IMPLEMENTATION
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

    struct ValidLifecycle;
    impl LifecycleImplementation for ValidLifecycle {
        const PRODUCT: &'static str = "loom";
        const IMPLEMENTATION: &'static str = "interactive";
    }

    struct InvalidLifecycle;
    impl LifecycleImplementation for InvalidLifecycle {
        const PRODUCT: &'static str = "loom";
        const IMPLEMENTATION: &'static str = "interactive generation";
    }

    #[test]
    fn lifecycle_and_component_identities_reject_ambiguous_or_unsafe_values() {
        assert!(ValidLifecycle::validate().is_ok());
        assert!(matches!(
            InvalidLifecycle::validate(),
            Err(AcceptanceError::InvalidIdentity {
                field: "implementation"
            })
        ));
        assert!(validate_id("component", "host").is_ok());
        assert!(validate_id("component", "bad\\component").is_err());
    }
}
