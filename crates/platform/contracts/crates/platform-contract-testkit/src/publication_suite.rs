use crate::fault::{FaultPoint, FaultScript};
use platform_contracts_v0::error::{
    ErrorClass, RetryAdvice, SERVICE_ERROR_SCHEMA_V0, ServiceErrorV0,
};
use platform_contracts_v0::ids::{ContentDigest, ServiceId};
use platform_contracts_v0::publication::{
    ArtifactIdentityV0, DestinationIdentityV0, PUBLICATION_RECEIPT_SCHEMA_V0, PublicationOutcomeV0,
    PublicationReceiptV0,
};
use std::collections::BTreeMap;
use std::fmt::Debug;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRequest {
    pub artifact: ArtifactIdentityV0,
    pub destination: DestinationIdentityV0,
}

impl PublicationRequest {
    pub fn validate(&self) {
        self.artifact
            .validate()
            .expect("valid publication artifact");
        self.destination
            .validate()
            .expect("valid publication destination");
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagingFacts {
    pub filesystem_id: String,
    pub private: bool,
    pub owned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationStep {
    InspectDestination,
    CreatePrivateSiblingStage,
    VerifyExactDigestAndLength,
    SyncStagedFile,
    PublishNoClobber,
    SyncVisibleFile,
    SyncParentDirectory,
    CleanupOwnedStage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationErrorContext {
    NotPublished,
    VisibleDurabilityUnknown,
}

/// Test-only adapter exposing each real publication boundary to the model suite.
pub trait PublicationModelAdapter {
    type Error: Debug;
    type Stage: Debug;

    fn inject_fault(&mut self, point: FaultPoint);
    fn seed_visible(&mut self, artifact: ArtifactIdentityV0);
    fn inspect_visible(
        &mut self,
        destination: &DestinationIdentityV0,
    ) -> Option<ArtifactIdentityV0>;
    fn create_private_sibling_stage(
        &mut self,
        request: &PublicationRequest,
    ) -> Result<Self::Stage, Self::Error>;
    fn staging_facts(&self, stage: &Self::Stage) -> StagingFacts;
    fn verify_exact(
        &mut self,
        stage: &Self::Stage,
        expected: &ArtifactIdentityV0,
    ) -> Result<(), Self::Error>;
    fn sync_staged_file(&mut self, stage: &Self::Stage) -> Result<(), Self::Error>;
    fn publish_no_clobber(
        &mut self,
        stage: &Self::Stage,
        destination: &DestinationIdentityV0,
    ) -> Result<(), Self::Error>;
    fn sync_visible_file(&mut self, destination: &DestinationIdentityV0)
    -> Result<(), Self::Error>;
    fn sync_parent_directory(
        &mut self,
        destination: &DestinationIdentityV0,
    ) -> Result<(), Self::Error>;
    fn cleanup_owned_stage(&mut self, stage: Self::Stage) -> Result<(), Self::Error>;
    fn service_error(
        &self,
        error: &Self::Error,
        context: PublicationErrorContext,
    ) -> ServiceErrorV0;
    fn conflict_error(&self) -> ServiceErrorV0;
    fn trace(&self) -> &[PublicationStep];
    fn visible_artifact(&self) -> Option<&ArtifactIdentityV0>;
    fn owned_staging_count(&self) -> usize;
}

/// Execute the publication state machine and return only canonical v0 outcomes.
/// Adapter contract violations panic because this function is a test oracle.
pub fn run_publication_model<A: PublicationModelAdapter>(
    adapter: &mut A,
    request: &PublicationRequest,
) -> PublicationOutcomeV0 {
    request.validate();
    if let Some(existing) = adapter.inspect_visible(&request.destination) {
        if !same_content(&existing, &request.artifact) {
            return validated(PublicationOutcomeV0::NotPublished {
                error: adapter.conflict_error(),
            });
        }
        adapter
            .sync_visible_file(&request.destination)
            .expect("exact-match recovery must resync the visible file");
        return match adapter.sync_parent_directory(&request.destination) {
            Ok(()) => validated(PublicationOutcomeV0::Published {
                receipt: receipt(request, true, true),
            }),
            Err(error) => durability_unknown(adapter, request, &error, true),
        };
    }

    let stage = adapter
        .create_private_sibling_stage(request)
        .expect("adapter must create a private sibling staging entry");
    let facts = adapter.staging_facts(&stage);
    assert!(facts.private, "staging entry must not be publicly visible");
    assert!(facts.owned, "adapter may clean up only staging it owns");
    assert_eq!(
        facts.filesystem_id, request.destination.filesystem_id,
        "staging and destination must share a filesystem"
    );
    if let Err(error) = adapter.verify_exact(&stage, &request.artifact) {
        cleanup(adapter, stage);
        return not_published(adapter, &error);
    }
    if let Err(error) = adapter.sync_staged_file(&stage) {
        cleanup(adapter, stage);
        return not_published(adapter, &error);
    }
    if let Err(error) = adapter.publish_no_clobber(&stage, &request.destination) {
        let visible = adapter.inspect_visible(&request.destination);
        cleanup(adapter, stage);
        return match visible {
            Some(artifact) if same_content(&artifact, &request.artifact) => {
                durability_unknown(adapter, request, &error, false)
            }
            Some(_) => validated(PublicationOutcomeV0::NotPublished {
                error: adapter.conflict_error(),
            }),
            None => not_published(adapter, &error),
        };
    }

    let visible = adapter
        .inspect_visible(&request.destination)
        .expect("successful no-clobber publication must be visible");
    assert!(
        same_content(&visible, &request.artifact),
        "successful no-clobber publication must expose the exact staged content"
    );
    let parent_result = adapter.sync_parent_directory(&request.destination);
    cleanup(adapter, stage);
    match parent_result {
        Ok(()) => validated(PublicationOutcomeV0::Published {
            receipt: receipt(request, false, true),
        }),
        Err(error) => durability_unknown(adapter, request, &error, false),
    }
}

/// Reusable acceptance assertions for a product adapter factory.
pub fn assert_publication_model<A, F>(mut factory: F, request: &PublicationRequest)
where
    A: PublicationModelAdapter,
    F: FnMut() -> A,
{
    let mut fresh = factory();
    assert_published(&run_publication_model(&mut fresh, request), false);
    assert_eq!(
        fresh.trace(),
        &[
            PublicationStep::InspectDestination,
            PublicationStep::CreatePrivateSiblingStage,
            PublicationStep::VerifyExactDigestAndLength,
            PublicationStep::SyncStagedFile,
            PublicationStep::PublishNoClobber,
            PublicationStep::InspectDestination,
            PublicationStep::SyncParentDirectory,
            PublicationStep::CleanupOwnedStage,
        ]
    );
    assert_eq!(fresh.owned_staging_count(), 0);

    let mut corrupt = factory();
    corrupt.inject_fault(FaultPoint::CorruptStagedBeforeVerification);
    assert_not_published(&run_publication_model(&mut corrupt, request));
    assert!(corrupt.visible_artifact().is_none());
    assert_eq!(corrupt.owned_staging_count(), 0);
    assert_eq!(
        corrupt.trace(),
        &[
            PublicationStep::InspectDestination,
            PublicationStep::CreatePrivateSiblingStage,
            PublicationStep::VerifyExactDigestAndLength,
            PublicationStep::CleanupOwnedStage,
        ]
    );

    let mut unsynced = factory();
    unsynced.inject_fault(FaultPoint::BeforeFileSync);
    assert_not_published(&run_publication_model(&mut unsynced, request));
    assert!(unsynced.visible_artifact().is_none());
    assert_eq!(unsynced.owned_staging_count(), 0);
    assert!(
        !unsynced
            .trace()
            .contains(&PublicationStep::PublishNoClobber)
    );

    let mut before_visibility = factory();
    before_visibility.inject_fault(FaultPoint::BeforeVisibility);
    assert_not_published(&run_publication_model(&mut before_visibility, request));
    assert!(before_visibility.visible_artifact().is_none());
    assert_eq!(before_visibility.owned_staging_count(), 0);

    let mut ambiguous = factory();
    ambiguous.inject_fault(FaultPoint::AfterNoClobberVisibility);
    assert_durability_unknown(&run_publication_model(&mut ambiguous, request), false);
    assert!(same_content(
        ambiguous.visible_artifact().expect("visible artifact"),
        &request.artifact
    ));
    assert_eq!(ambiguous.owned_staging_count(), 0);

    let mut parent_sync = factory();
    parent_sync.inject_fault(FaultPoint::BeforeParentSync);
    assert_durability_unknown(&run_publication_model(&mut parent_sync, request), false);
    assert!(same_content(
        parent_sync.visible_artifact().expect("visible artifact"),
        &request.artifact
    ));

    let mut recovery = factory();
    recovery.seed_visible(request.artifact.clone());
    recovery.inject_fault(FaultPoint::BeforeParentSync);
    assert_durability_unknown(&run_publication_model(&mut recovery, request), true);
    assert_published(&run_publication_model(&mut recovery, request), true);
    assert_eq!(recovery.owned_staging_count(), 0);
    assert_eq!(
        recovery
            .trace()
            .iter()
            .filter(|step| **step == PublicationStep::SyncVisibleFile)
            .count(),
        2,
        "each exact-match recovery must resync the visible file"
    );

    let mut conflict = factory();
    let conflicting = ArtifactIdentityV0 {
        id: "conflicting-artifact".to_owned(),
        digest: ContentDigest::sha256("f".repeat(64)).expect("static digest"),
        length: request.artifact.length.saturating_add(1),
    };
    conflict.seed_visible(conflicting.clone());
    assert_not_published(&run_publication_model(&mut conflict, request));
    assert_eq!(conflict.visible_artifact(), Some(&conflicting));
    assert_eq!(conflict.owned_staging_count(), 0);
    assert_eq!(conflict.trace(), &[PublicationStep::InspectDestination]);
}

fn same_content(left: &ArtifactIdentityV0, right: &ArtifactIdentityV0) -> bool {
    left.digest == right.digest && left.length == right.length
}

fn receipt(
    request: &PublicationRequest,
    idempotent_recovery: bool,
    directory_synced: bool,
) -> PublicationReceiptV0 {
    PublicationReceiptV0 {
        schema: PUBLICATION_RECEIPT_SCHEMA_V0.to_owned(),
        artifact: request.artifact.clone(),
        destination: request.destination.clone(),
        visible: true,
        file_synced: true,
        directory_synced,
        idempotent_recovery,
    }
}

fn not_published<A: PublicationModelAdapter>(
    adapter: &A,
    error: &A::Error,
) -> PublicationOutcomeV0 {
    validated(PublicationOutcomeV0::NotPublished {
        error: adapter.service_error(error, PublicationErrorContext::NotPublished),
    })
}

fn durability_unknown<A: PublicationModelAdapter>(
    adapter: &A,
    request: &PublicationRequest,
    error: &A::Error,
    idempotent_recovery: bool,
) -> PublicationOutcomeV0 {
    validated(PublicationOutcomeV0::PublishedDurabilityUnknown {
        receipt: receipt(request, idempotent_recovery, false),
        error: adapter.service_error(error, PublicationErrorContext::VisibleDurabilityUnknown),
    })
}

fn cleanup<A: PublicationModelAdapter>(adapter: &mut A, stage: A::Stage) {
    adapter
        .cleanup_owned_stage(stage)
        .expect("owned staging cleanup must succeed");
}

fn validated(outcome: PublicationOutcomeV0) -> PublicationOutcomeV0 {
    outcome.validate().expect("canonical publication outcome");
    outcome
}

fn assert_not_published(outcome: &PublicationOutcomeV0) {
    assert!(matches!(outcome, PublicationOutcomeV0::NotPublished { .. }));
    outcome.validate().expect("valid not-published outcome");
}

fn assert_published(outcome: &PublicationOutcomeV0, idempotent_recovery: bool) {
    let PublicationOutcomeV0::Published { receipt } = outcome else {
        panic!("expected published outcome, got {outcome:?}");
    };
    assert_eq!(receipt.idempotent_recovery, idempotent_recovery);
    outcome.validate().expect("valid published outcome");
}

fn assert_durability_unknown(outcome: &PublicationOutcomeV0, idempotent_recovery: bool) {
    let PublicationOutcomeV0::PublishedDurabilityUnknown { receipt, .. } = outcome else {
        panic!("expected durability-unknown outcome, got {outcome:?}");
    };
    assert_eq!(receipt.idempotent_recovery, idempotent_recovery);
    outcome
        .validate()
        .expect("valid durability-unknown outcome");
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReferenceStage(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferencePublicationError {
    Injected(FaultPoint),
    VerificationMismatch,
    DestinationConflict,
    FileNotSynced,
    UnknownStage,
}

#[derive(Clone, Debug)]
struct StagedArtifact {
    artifact: ArtifactIdentityV0,
    facts: StagingFacts,
    file_synced: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReferencePublicationAdapter {
    faults: FaultScript,
    next_stage: u64,
    stages: BTreeMap<ReferenceStage, StagedArtifact>,
    visible: Option<ArtifactIdentityV0>,
    trace: Vec<PublicationStep>,
}

impl ReferencePublicationAdapter {
    fn hit(&mut self, point: FaultPoint) -> Result<(), ReferencePublicationError> {
        self.faults
            .hit(point)
            .map_err(|fault| ReferencePublicationError::Injected(fault.0))
    }
}

impl PublicationModelAdapter for ReferencePublicationAdapter {
    type Error = ReferencePublicationError;
    type Stage = ReferenceStage;

    fn inject_fault(&mut self, point: FaultPoint) {
        self.faults = FaultScript::new([point]);
    }

    fn seed_visible(&mut self, artifact: ArtifactIdentityV0) {
        assert!(self.visible.is_none(), "destination already seeded");
        self.visible = Some(artifact);
    }

    fn inspect_visible(
        &mut self,
        _destination: &DestinationIdentityV0,
    ) -> Option<ArtifactIdentityV0> {
        self.trace.push(PublicationStep::InspectDestination);
        self.visible.clone()
    }

    fn create_private_sibling_stage(
        &mut self,
        request: &PublicationRequest,
    ) -> Result<Self::Stage, Self::Error> {
        self.trace.push(PublicationStep::CreatePrivateSiblingStage);
        self.next_stage = self.next_stage.checked_add(1).expect("stage ID overflow");
        let stage = ReferenceStage(self.next_stage);
        let previous = self.stages.insert(
            stage,
            StagedArtifact {
                artifact: request.artifact.clone(),
                facts: StagingFacts {
                    filesystem_id: request.destination.filesystem_id.clone(),
                    private: true,
                    owned: true,
                },
                file_synced: false,
            },
        );
        assert!(previous.is_none(), "stage ID must be unique");
        Ok(stage)
    }

    fn staging_facts(&self, stage: &Self::Stage) -> StagingFacts {
        self.stages.get(stage).expect("known stage").facts.clone()
    }

    fn verify_exact(
        &mut self,
        stage: &Self::Stage,
        expected: &ArtifactIdentityV0,
    ) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::VerifyExactDigestAndLength);
        if self
            .hit(FaultPoint::CorruptStagedBeforeVerification)
            .is_err()
        {
            let staged = self
                .stages
                .get_mut(stage)
                .ok_or(ReferencePublicationError::UnknownStage)?;
            staged.artifact.digest = ContentDigest::sha256("e".repeat(64)).expect("static digest");
            staged.artifact.length = staged.artifact.length.saturating_add(1);
        }
        let staged = self
            .stages
            .get(stage)
            .ok_or(ReferencePublicationError::UnknownStage)?;
        if same_content(&staged.artifact, expected) {
            Ok(())
        } else {
            Err(ReferencePublicationError::VerificationMismatch)
        }
    }

    fn sync_staged_file(&mut self, stage: &Self::Stage) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::SyncStagedFile);
        self.hit(FaultPoint::BeforeFileSync)?;
        self.stages
            .get_mut(stage)
            .ok_or(ReferencePublicationError::UnknownStage)?
            .file_synced = true;
        Ok(())
    }

    fn publish_no_clobber(
        &mut self,
        stage: &Self::Stage,
        _destination: &DestinationIdentityV0,
    ) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::PublishNoClobber);
        let staged = self
            .stages
            .get(stage)
            .ok_or(ReferencePublicationError::UnknownStage)?;
        if !staged.file_synced {
            return Err(ReferencePublicationError::FileNotSynced);
        }
        if self.visible.is_some() {
            return Err(ReferencePublicationError::DestinationConflict);
        }
        let artifact = staged.artifact.clone();
        self.hit(FaultPoint::BeforeVisibility)?;
        self.visible = Some(artifact);
        self.hit(FaultPoint::AfterNoClobberVisibility)
    }

    fn sync_visible_file(
        &mut self,
        _destination: &DestinationIdentityV0,
    ) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::SyncVisibleFile);
        assert!(self.visible.is_some(), "visible file must exist");
        Ok(())
    }

    fn sync_parent_directory(
        &mut self,
        _destination: &DestinationIdentityV0,
    ) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::SyncParentDirectory);
        self.hit(FaultPoint::BeforeParentSync)?;
        self.hit(FaultPoint::AfterVisibilityBeforeParentSync)
    }

    fn cleanup_owned_stage(&mut self, stage: Self::Stage) -> Result<(), Self::Error> {
        self.trace.push(PublicationStep::CleanupOwnedStage);
        self.stages
            .remove(&stage)
            .ok_or(ReferencePublicationError::UnknownStage)?;
        Ok(())
    }

    fn service_error(
        &self,
        error: &Self::Error,
        context: PublicationErrorContext,
    ) -> ServiceErrorV0 {
        let (code, class, retry, detail) = match context {
            PublicationErrorContext::VisibleDurabilityUnknown => (
                "publication.durability_unknown",
                ErrorClass::Publication,
                RetryAdvice::AfterRestart,
                "the exact artifact is visible but parent durability is unknown",
            ),
            PublicationErrorContext::NotPublished => match error {
                ReferencePublicationError::VerificationMismatch => (
                    "publication.verification_mismatch",
                    ErrorClass::Integrity,
                    RetryAdvice::Never,
                    "staged digest or length did not match the requested artifact",
                ),
                _ => (
                    "publication.not_visible",
                    ErrorClass::Storage,
                    RetryAdvice::Immediate,
                    "the requested artifact did not become visible",
                ),
            },
        };
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: code.to_owned(),
            class,
            retry,
            operation_id: None,
            service: ServiceId::new("publication-testkit").expect("static service ID"),
            safe_detail: detail.to_owned(),
        }
    }

    fn conflict_error(&self) -> ServiceErrorV0 {
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: "publication.destination_conflict".to_owned(),
            class: ErrorClass::Publication,
            retry: RetryAdvice::Never,
            operation_id: None,
            service: ServiceId::new("publication-testkit").expect("static service ID"),
            safe_detail: "a different artifact already occupies the destination".to_owned(),
        }
    }

    fn trace(&self) -> &[PublicationStep] {
        &self.trace
    }

    fn visible_artifact(&self) -> Option<&ArtifactIdentityV0> {
        self.visible.as_ref()
    }

    fn owned_staging_count(&self) -> usize {
        self.stages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PublicationRequest {
        PublicationRequest {
            artifact: ArtifactIdentityV0 {
                id: "artifact-1".to_owned(),
                digest: ContentDigest::sha256("a".repeat(64)).expect("static digest"),
                length: 17,
            },
            destination: DestinationIdentityV0 {
                filesystem_id: "filesystem-1".to_owned(),
                path_id: "manuscript.md".to_owned(),
            },
        }
    }

    #[test]
    fn reference_adapter_satisfies_the_generic_publication_suite() {
        assert_publication_model(ReferencePublicationAdapter::default, &request());
    }
}
