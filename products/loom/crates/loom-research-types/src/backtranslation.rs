use std::collections::BTreeSet;

use loom_types::{BlobId, RevisionId};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BoundError, BoundedVec, NonEmptyBoundedVec, PromptSourceRange, RangeError, TrialCaseId,
};

pub const MAX_BACKTRANSLATION_ROLES: usize = 64;
pub const MAX_BACKTRANSLATION_OBJECTS: usize = 128;
pub const MAX_BACKTRANSLATION_FIELDS_PER_SECTION: usize = 256;
pub const MAX_BACKTRANSLATION_FIELD_ARGUMENTS: usize = 16;
pub const MAX_BACKTRANSLATION_EVIDENCE_SPANS: usize = 16;
pub const MIN_BACKTRANSLATION_AUDITION_CASES: usize = 2;
pub const MAX_BACKTRANSLATION_AUDITION_CASES: usize = 32;

const PROPOSAL_FINGERPRINT_DOMAIN: &[u8] = b"loom/backtranslation-proposal/v1\0";
const AUDITION_FINGERPRINT_DOMAIN: &[u8] = b"loom/backtranslation-audition/v1\0";
const ACCEPTED_FINGERPRINT_DOMAIN: &[u8] = b"loom/auditioned-backtranslation/v1\0";

/// A source-free reference replacing a named person or distinctive object.
///
/// The ordinal has meaning only within one proposal. No character or object
/// name is stored in this artifact.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypedPlaceholder {
    kind: PlaceholderKind,
    ordinal: u16,
}

impl TypedPlaceholder {
    pub const fn role(ordinal: u16) -> Self {
        Self {
            kind: PlaceholderKind::Role,
            ordinal,
        }
    }

    pub const fn object(ordinal: u16) -> Self {
        Self {
            kind: PlaceholderKind::Object,
            ordinal,
        }
    }

    pub const fn kind(self) -> PlaceholderKind {
        self.kind
    }

    pub const fn ordinal(self) -> u16 {
        self.ordinal
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update([self.kind.domain_tag()]);
        digest.update(self.ordinal.to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderKind {
    Role,
    Object,
}

impl PlaceholderKind {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::Role => 0,
            Self::Object => 1,
        }
    }
}

/// A coarse narrative function, never a surface character name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrativeRole {
    Viewpoint,
    Counterparty,
    Ally,
    Rival,
    Authority,
    Dependent,
    Witness,
    Other,
}

impl NarrativeRole {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::Viewpoint => 0,
            Self::Counterparty => 1,
            Self::Ally => 2,
            Self::Rival => 3,
            Self::Authority => 4,
            Self::Dependent => 5,
            Self::Witness => 6,
            Self::Other => 7,
        }
    }
}

/// A term in an immutable external semantic ontology.
///
/// A numeric code avoids copying names, dialogue, or distinctive source
/// wording into the backtranslation. Its meaning is bound by the proposal's
/// ontology fingerprint and must be interpreted by that exact ontology.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SemanticTermCode(u32);

impl SemanticTermCode {
    pub const fn new(value: u32) -> Result<Self, BacktranslationError> {
        if value == 0 {
            return Err(BacktranslationError::ZeroSemanticTerm);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for SemanticTermCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacktranslationRole {
    placeholder: TypedPlaceholder,
    narrative_role: NarrativeRole,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl BacktranslationRole {
    pub fn new(
        placeholder: TypedPlaceholder,
        narrative_role: NarrativeRole,
        evidence: Vec<PromptSourceRange>,
    ) -> Result<Self, BacktranslationError> {
        if placeholder.kind() != PlaceholderKind::Role {
            return Err(BacktranslationError::WrongPlaceholderKind {
                expected: PlaceholderKind::Role,
                actual: placeholder.kind(),
            });
        }
        let evidence = bounded_unique_evidence(evidence)?;
        Ok(Self {
            placeholder,
            narrative_role,
            evidence,
        })
    }

    pub const fn placeholder(&self) -> TypedPlaceholder {
        self.placeholder
    }

    pub const fn narrative_role(&self) -> NarrativeRole {
        self.narrative_role
    }

    pub fn evidence(&self) -> &[PromptSourceRange] {
        &self.evidence
    }

    fn update_digest(&self, digest: &mut Sha256) {
        self.placeholder.update_digest(digest);
        digest.update([self.narrative_role.domain_tag()]);
        update_evidence_digest(&self.evidence, digest);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktranslationRoleWire {
    placeholder: TypedPlaceholder,
    narrative_role: NarrativeRole,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl<'de> Deserialize<'de> for BacktranslationRole {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BacktranslationRoleWire::deserialize(deserializer)?;
        Self::new(
            wire.placeholder,
            wire.narrative_role,
            wire.evidence.into_inner(),
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacktranslationObject {
    placeholder: TypedPlaceholder,
    class: SemanticTermCode,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl BacktranslationObject {
    pub fn new(
        placeholder: TypedPlaceholder,
        class: SemanticTermCode,
        evidence: Vec<PromptSourceRange>,
    ) -> Result<Self, BacktranslationError> {
        if placeholder.kind() != PlaceholderKind::Object {
            return Err(BacktranslationError::WrongPlaceholderKind {
                expected: PlaceholderKind::Object,
                actual: placeholder.kind(),
            });
        }
        let evidence = bounded_unique_evidence(evidence)?;
        Ok(Self {
            placeholder,
            class,
            evidence,
        })
    }

    pub const fn placeholder(&self) -> TypedPlaceholder {
        self.placeholder
    }

    pub const fn class(&self) -> SemanticTermCode {
        self.class
    }

    pub fn evidence(&self) -> &[PromptSourceRange] {
        &self.evidence
    }

    fn update_digest(&self, digest: &mut Sha256) {
        self.placeholder.update_digest(digest);
        digest.update(self.class.get().to_be_bytes());
        update_evidence_digest(&self.evidence, digest);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktranslationObjectWire {
    placeholder: TypedPlaceholder,
    class: SemanticTermCode,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl<'de> Deserialize<'de> for BacktranslationObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BacktranslationObjectWire::deserialize(deserializer)?;
        Self::new(wire.placeholder, wire.class, wire.evidence.into_inner())
            .map_err(serde::de::Error::custom)
    }
}

/// One controller-proposed semantic relation with exact source evidence.
///
/// Construction proves only structural binding. It does not prove that the
/// relation is a correct reading of the cited source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GroundedBacktranslationField {
    field_id: u32,
    subject: TypedPlaceholder,
    relation: SemanticTermCode,
    arguments: BoundedVec<TypedPlaceholder, MAX_BACKTRANSLATION_FIELD_ARGUMENTS>,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl GroundedBacktranslationField {
    pub fn new(
        field_id: u32,
        subject: TypedPlaceholder,
        relation: SemanticTermCode,
        arguments: Vec<TypedPlaceholder>,
        evidence: Vec<PromptSourceRange>,
    ) -> Result<Self, BacktranslationError> {
        if field_id == 0 {
            return Err(BacktranslationError::ZeroFieldId);
        }
        let arguments = BoundedVec::new(arguments)?;
        let mut unique_arguments = BTreeSet::new();
        if arguments
            .iter()
            .copied()
            .any(|argument| !unique_arguments.insert(argument))
        {
            return Err(BacktranslationError::DuplicateFieldArgument(field_id));
        }
        let evidence = bounded_unique_evidence(evidence)?;
        Ok(Self {
            field_id,
            subject,
            relation,
            arguments,
            evidence,
        })
    }

    pub const fn field_id(&self) -> u32 {
        self.field_id
    }

    pub const fn subject(&self) -> TypedPlaceholder {
        self.subject
    }

    pub const fn relation(&self) -> SemanticTermCode {
        self.relation
    }

    pub fn arguments(&self) -> &[TypedPlaceholder] {
        &self.arguments
    }

    pub fn evidence(&self) -> &[PromptSourceRange] {
        &self.evidence
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.field_id.to_be_bytes());
        self.subject.update_digest(digest);
        digest.update(self.relation.get().to_be_bytes());
        digest.update((self.arguments.len() as u64).to_be_bytes());
        for argument in self.arguments.iter().copied() {
            argument.update_digest(digest);
        }
        update_evidence_digest(&self.evidence, digest);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroundedBacktranslationFieldWire {
    field_id: u32,
    subject: TypedPlaceholder,
    relation: SemanticTermCode,
    arguments: BoundedVec<TypedPlaceholder, MAX_BACKTRANSLATION_FIELD_ARGUMENTS>,
    evidence: NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
}

impl<'de> Deserialize<'de> for GroundedBacktranslationField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GroundedBacktranslationFieldWire::deserialize(deserializer)?;
        Self::new(
            wire.field_id,
            wire.subject,
            wire.relation,
            wire.arguments.into_inner(),
            wire.evidence.into_inner(),
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BacktranslationAbstentionReason {
    NoObservableEvidence,
    AmbiguousEvidence,
    ControllerUnsupported,
    EvidenceConflict,
}

impl BacktranslationAbstentionReason {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::NoObservableEvidence => 0,
            Self::AmbiguousEvidence => 1,
            Self::ControllerUnsupported => 2,
            Self::EvidenceConflict => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
pub enum BacktranslationSection {
    Grounded {
        fields: NonEmptyBoundedVec<
            GroundedBacktranslationField,
            MAX_BACKTRANSLATION_FIELDS_PER_SECTION,
        >,
    },
    Abstained {
        reason: BacktranslationAbstentionReason,
    },
}

impl BacktranslationSection {
    pub fn grounded(
        fields: Vec<GroundedBacktranslationField>,
    ) -> Result<Self, BacktranslationError> {
        let fields = NonEmptyBoundedVec::new(fields)?;
        let mut ids = BTreeSet::new();
        for field in fields.iter() {
            if !ids.insert(field.field_id()) {
                return Err(BacktranslationError::DuplicateFieldId(field.field_id()));
            }
        }
        Ok(Self::Grounded { fields })
    }

    pub const fn abstained(reason: BacktranslationAbstentionReason) -> Self {
        Self::Abstained { reason }
    }

    pub fn fields(&self) -> &[GroundedBacktranslationField] {
        match self {
            Self::Grounded { fields } => fields,
            Self::Abstained { .. } => &[],
        }
    }

    pub const fn abstention_reason(&self) -> Option<BacktranslationAbstentionReason> {
        match self {
            Self::Grounded { .. } => None,
            Self::Abstained { reason } => Some(*reason),
        }
    }

    fn update_digest(&self, digest: &mut Sha256) {
        match self {
            Self::Grounded { fields } => {
                digest.update([0]);
                digest.update((fields.len() as u64).to_be_bytes());
                for field in fields.iter() {
                    field.update_digest(digest);
                }
            }
            Self::Abstained { reason } => {
                digest.update([1, reason.domain_tag()]);
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
enum BacktranslationSectionWire {
    Grounded {
        fields: NonEmptyBoundedVec<
            GroundedBacktranslationField,
            MAX_BACKTRANSLATION_FIELDS_PER_SECTION,
        >,
    },
    Abstained {
        reason: BacktranslationAbstentionReason,
    },
}

impl<'de> Deserialize<'de> for BacktranslationSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match BacktranslationSectionWire::deserialize(deserializer)? {
            BacktranslationSectionWire::Grounded { fields } => Self::grounded(fields.into_inner()),
            BacktranslationSectionWire::Abstained { reason } => Ok(Self::abstained(reason)),
        }
        .map_err(serde::de::Error::custom)
    }
}

/// The six required extraction channels. Each is either grounded or an
/// explicit typed abstention; missing channels cannot deserialize.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktranslationSections {
    pub causal_events: BacktranslationSection,
    pub knowledge_changes: BacktranslationSection,
    pub objects: BacktranslationSection,
    pub physical_positions: BacktranslationSection,
    pub dialogue_tactics: BacktranslationSection,
    pub resulting_state: BacktranslationSection,
}

impl BacktranslationSections {
    fn for_each(&self, mut visit: impl FnMut(u8, &BacktranslationSection)) {
        visit(0, &self.causal_events);
        visit(1, &self.knowledge_changes);
        visit(2, &self.objects);
        visit(3, &self.physical_positions);
        visit(4, &self.dialogue_tactics);
        visit(5, &self.resulting_state);
    }

    fn update_digest(&self, digest: &mut Sha256) {
        self.for_each(|tag, section| {
            digest.update([tag]);
            section.update_digest(digest);
        });
    }
}

/// A controller proposal tied to one exact source range.
///
/// This value is diagnostic and nonauthorizing. `verify_source` proves exact
/// byte binding, not semantic correctness. It becomes structurally eligible
/// for demonstration only through `audition`, whose external receipts still
/// require independent backend/evaluator validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacktranslationProposal {
    source: PromptSourceRange,
    source_work_fingerprint: BlobId,
    controller_model_fingerprint: BlobId,
    controller_prompt_fingerprint: BlobId,
    controller_call_fingerprint: BlobId,
    ontology_fingerprint: BlobId,
    roles: BoundedVec<BacktranslationRole, MAX_BACKTRANSLATION_ROLES>,
    objects: BoundedVec<BacktranslationObject, MAX_BACKTRANSLATION_OBJECTS>,
    sections: BacktranslationSections,
    fingerprint: BlobId,
}

#[allow(clippy::too_many_arguments)]
impl BacktranslationProposal {
    pub fn new(
        source: PromptSourceRange,
        source_work_fingerprint: BlobId,
        controller_model_fingerprint: BlobId,
        controller_prompt_fingerprint: BlobId,
        controller_call_fingerprint: BlobId,
        ontology_fingerprint: BlobId,
        roles: Vec<BacktranslationRole>,
        objects: Vec<BacktranslationObject>,
        sections: BacktranslationSections,
    ) -> Result<Self, BacktranslationError> {
        let roles = BoundedVec::new(roles)?;
        let objects = BoundedVec::new(objects)?;
        let mut proposal = Self {
            source,
            source_work_fingerprint,
            controller_model_fingerprint,
            controller_prompt_fingerprint,
            controller_call_fingerprint,
            ontology_fingerprint,
            roles,
            objects,
            sections,
            fingerprint: BlobId::digest(&[]),
        };
        proposal.validate_structure()?;
        proposal.fingerprint = proposal.compute_fingerprint();
        Ok(proposal)
    }

    pub const fn source(&self) -> PromptSourceRange {
        self.source
    }

    pub const fn source_work_fingerprint(&self) -> BlobId {
        self.source_work_fingerprint
    }

    pub const fn controller_model_fingerprint(&self) -> BlobId {
        self.controller_model_fingerprint
    }

    pub const fn controller_prompt_fingerprint(&self) -> BlobId {
        self.controller_prompt_fingerprint
    }

    pub const fn controller_call_fingerprint(&self) -> BlobId {
        self.controller_call_fingerprint
    }

    pub const fn ontology_fingerprint(&self) -> BlobId {
        self.ontology_fingerprint
    }

    pub fn roles(&self) -> &[BacktranslationRole] {
        &self.roles
    }

    pub fn objects(&self) -> &[BacktranslationObject] {
        &self.objects
    }

    pub const fn sections(&self) -> &BacktranslationSections {
        &self.sections
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Revalidates all evidence against one exact immutable source revision.
    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        exact_source_bytes: &[u8],
    ) -> Result<(), BacktranslationError> {
        if self.source.revision_id() != expected_revision_id {
            return Err(BacktranslationError::SourceRevisionMismatch {
                expected: expected_revision_id,
                actual: self.source.revision_id(),
            });
        }
        crate::range::validate_source_utf8(exact_source_bytes)?;
        let actual = BlobId::digest(exact_source_bytes);
        if actual != self.source.source_blob_id() {
            return Err(BacktranslationError::SourceBlobMismatch {
                expected: self.source.source_blob_id(),
                actual,
            });
        }
        let _ = self.source.range().checked_str(exact_source_bytes)?;
        for evidence in self.all_evidence() {
            validate_evidence_in_source(self.source, evidence)?;
            let _ = evidence.range().checked_str(exact_source_bytes)?;
        }
        Ok(())
    }

    pub fn audition(
        self,
        receipt: BacktranslationAuditionReceipt,
    ) -> Result<AuditionedBacktranslation, BacktranslationError> {
        if receipt.proposal_fingerprint != self.fingerprint {
            return Err(BacktranslationError::AuditionProposalMismatch);
        }
        if receipt.source_work_fingerprint != self.source_work_fingerprint {
            return Err(BacktranslationError::AuditionSourceWorkMismatch);
        }
        if receipt.controller_call_fingerprint != self.controller_call_fingerprint {
            return Err(BacktranslationError::AuditionControllerCallMismatch);
        }
        if receipt.cases.iter().any(|case| {
            case.source().source_blob_id() == self.source.source_blob_id()
                || case.source().revision_id() == self.source.revision_id()
        }) {
            return Err(BacktranslationError::AuditionReusesProposalSource);
        }
        if receipt.cases.iter().any(|case| {
            case.improvement != CausalTransferDecision::Improved
                || case.leakage != LeakageDecision::Clear
        }) {
            return Err(BacktranslationError::AuditionDidNotPass);
        }
        let fingerprint = fingerprint_accepted(self.fingerprint, receipt.fingerprint);
        Ok(AuditionedBacktranslation {
            proposal: self,
            receipt,
            fingerprint,
        })
    }

    fn all_evidence(&self) -> Vec<PromptSourceRange> {
        let mut evidence = Vec::new();
        for role in self.roles.iter() {
            evidence.extend_from_slice(role.evidence());
        }
        for object in self.objects.iter() {
            evidence.extend_from_slice(object.evidence());
        }
        self.sections.for_each(|_, section| {
            for field in section.fields() {
                evidence.extend_from_slice(field.evidence());
            }
        });
        evidence
    }

    fn validate_structure(&self) -> Result<(), BacktranslationError> {
        let mut declared = BTreeSet::new();
        for role in self.roles.iter() {
            if !declared.insert(role.placeholder()) {
                return Err(BacktranslationError::DuplicatePlaceholder(
                    role.placeholder(),
                ));
            }
            validate_evidence_set_in_source(self.source, role.evidence())?;
        }
        for object in self.objects.iter() {
            if !declared.insert(object.placeholder()) {
                return Err(BacktranslationError::DuplicatePlaceholder(
                    object.placeholder(),
                ));
            }
            validate_evidence_set_in_source(self.source, object.evidence())?;
        }

        let mut field_ids = BTreeSet::new();
        let mut error = None;
        self.sections.for_each(|_, section| {
            for field in section.fields() {
                if error.is_some() {
                    return;
                }
                if !field_ids.insert(field.field_id()) {
                    error = Some(BacktranslationError::DuplicateFieldId(field.field_id()));
                    return;
                }
                for referenced in
                    std::iter::once(field.subject()).chain(field.arguments().iter().copied())
                {
                    if !declared.contains(&referenced) {
                        error = Some(BacktranslationError::UndeclaredPlaceholder(referenced));
                        return;
                    }
                }
                if let Err(source_error) =
                    validate_evidence_set_in_source(self.source, field.evidence())
                {
                    error = Some(source_error);
                }
            }
        });
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(PROPOSAL_FINGERPRINT_DOMAIN);
        update_source_digest(self.source, &mut digest);
        digest.update(self.source_work_fingerprint.as_bytes());
        digest.update(self.controller_model_fingerprint.as_bytes());
        digest.update(self.controller_prompt_fingerprint.as_bytes());
        digest.update(self.controller_call_fingerprint.as_bytes());
        digest.update(self.ontology_fingerprint.as_bytes());
        digest.update((self.roles.len() as u64).to_be_bytes());
        for role in self.roles.iter() {
            role.update_digest(&mut digest);
        }
        digest.update((self.objects.len() as u64).to_be_bytes());
        for object in self.objects.iter() {
            object.update_digest(&mut digest);
        }
        self.sections.update_digest(&mut digest);
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktranslationProposalWire {
    source: PromptSourceRange,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_work_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    controller_model_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    controller_prompt_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    controller_call_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    ontology_fingerprint: BlobId,
    roles: BoundedVec<BacktranslationRole, MAX_BACKTRANSLATION_ROLES>,
    objects: BoundedVec<BacktranslationObject, MAX_BACKTRANSLATION_OBJECTS>,
    sections: BacktranslationSections,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for BacktranslationProposal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BacktranslationProposalWire::deserialize(deserializer)?;
        let proposal = Self::new(
            wire.source,
            wire.source_work_fingerprint,
            wire.controller_model_fingerprint,
            wire.controller_prompt_fingerprint,
            wire.controller_call_fingerprint,
            wire.ontology_fingerprint,
            wire.roles.into_inner(),
            wire.objects.into_inner(),
            wire.sections,
        )
        .map_err(serde::de::Error::custom)?;
        if proposal.fingerprint != wire.fingerprint {
            return Err(serde::de::Error::custom(
                BacktranslationError::ProposalFingerprintMismatch,
            ));
        }
        Ok(proposal)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalTransferDecision {
    Improved,
    NotImproved,
    Abstained,
}

impl CausalTransferDecision {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::Improved => 0,
            Self::NotImproved => 1,
            Self::Abstained => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakageDecision {
    Clear,
    Detected,
    Abstained,
}

impl LeakageDecision {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::Clear => 0,
            Self::Detected => 1,
            Self::Abstained => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacktranslationAuditionCase {
    case_id: TrialCaseId,
    work_fingerprint: BlobId,
    source: PromptSourceRange,
    writer_model_fingerprint: BlobId,
    writer_tokenizer_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    call_fingerprint: BlobId,
    raw_output_blob_id: BlobId,
    selected_output_blob_id: BlobId,
    baseline_call_fingerprint: BlobId,
    baseline_output_blob_id: BlobId,
    evaluator_receipt_fingerprint: BlobId,
    improvement: CausalTransferDecision,
    leakage: LeakageDecision,
}

#[allow(clippy::too_many_arguments)]
impl BacktranslationAuditionCase {
    pub const fn new(
        case_id: TrialCaseId,
        work_fingerprint: BlobId,
        source: PromptSourceRange,
        writer_model_fingerprint: BlobId,
        writer_tokenizer_fingerprint: BlobId,
        prompt_fingerprint: BlobId,
        call_fingerprint: BlobId,
        raw_output_blob_id: BlobId,
        selected_output_blob_id: BlobId,
        baseline_call_fingerprint: BlobId,
        baseline_output_blob_id: BlobId,
        evaluator_receipt_fingerprint: BlobId,
        improvement: CausalTransferDecision,
        leakage: LeakageDecision,
    ) -> Self {
        Self {
            case_id,
            work_fingerprint,
            source,
            writer_model_fingerprint,
            writer_tokenizer_fingerprint,
            prompt_fingerprint,
            call_fingerprint,
            raw_output_blob_id,
            selected_output_blob_id,
            baseline_call_fingerprint,
            baseline_output_blob_id,
            evaluator_receipt_fingerprint,
            improvement,
            leakage,
        }
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn work_fingerprint(&self) -> BlobId {
        self.work_fingerprint
    }

    pub const fn source(&self) -> PromptSourceRange {
        self.source
    }

    pub const fn writer_model_fingerprint(&self) -> BlobId {
        self.writer_model_fingerprint
    }

    pub const fn writer_tokenizer_fingerprint(&self) -> BlobId {
        self.writer_tokenizer_fingerprint
    }

    pub const fn prompt_fingerprint(&self) -> BlobId {
        self.prompt_fingerprint
    }

    pub const fn call_fingerprint(&self) -> BlobId {
        self.call_fingerprint
    }

    pub const fn raw_output_blob_id(&self) -> BlobId {
        self.raw_output_blob_id
    }

    pub const fn selected_output_blob_id(&self) -> BlobId {
        self.selected_output_blob_id
    }

    pub const fn baseline_call_fingerprint(&self) -> BlobId {
        self.baseline_call_fingerprint
    }

    pub const fn baseline_output_blob_id(&self) -> BlobId {
        self.baseline_output_blob_id
    }

    pub const fn evaluator_receipt_fingerprint(&self) -> BlobId {
        self.evaluator_receipt_fingerprint
    }

    pub const fn improvement(&self) -> CausalTransferDecision {
        self.improvement
    }

    pub const fn leakage(&self) -> LeakageDecision {
        self.leakage
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.case_id.as_ulid().to_bytes());
        digest.update(self.work_fingerprint.as_bytes());
        update_source_digest(self.source, digest);
        digest.update(self.writer_model_fingerprint.as_bytes());
        digest.update(self.writer_tokenizer_fingerprint.as_bytes());
        digest.update(self.prompt_fingerprint.as_bytes());
        digest.update(self.call_fingerprint.as_bytes());
        digest.update(self.raw_output_blob_id.as_bytes());
        digest.update(self.selected_output_blob_id.as_bytes());
        digest.update(self.baseline_call_fingerprint.as_bytes());
        digest.update(self.baseline_output_blob_id.as_bytes());
        digest.update(self.evaluator_receipt_fingerprint.as_bytes());
        digest.update([self.improvement.domain_tag(), self.leakage.domain_tag()]);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktranslationAuditionCaseWire {
    case_id: TrialCaseId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    work_fingerprint: BlobId,
    source: PromptSourceRange,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    writer_model_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    writer_tokenizer_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    prompt_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    call_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_output_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    selected_output_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    baseline_call_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    baseline_output_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    evaluator_receipt_fingerprint: BlobId,
    improvement: CausalTransferDecision,
    leakage: LeakageDecision,
}

impl<'de> Deserialize<'de> for BacktranslationAuditionCase {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BacktranslationAuditionCaseWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.case_id,
            wire.work_fingerprint,
            wire.source,
            wire.writer_model_fingerprint,
            wire.writer_tokenizer_fingerprint,
            wire.prompt_fingerprint,
            wire.call_fingerprint,
            wire.raw_output_blob_id,
            wire.selected_output_blob_id,
            wire.baseline_call_fingerprint,
            wire.baseline_output_blob_id,
            wire.evaluator_receipt_fingerprint,
            wire.improvement,
            wire.leakage,
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacktranslationAuditionReceipt {
    proposal_fingerprint: BlobId,
    source_work_fingerprint: BlobId,
    controller_call_fingerprint: BlobId,
    cases: NonEmptyBoundedVec<BacktranslationAuditionCase, MAX_BACKTRANSLATION_AUDITION_CASES>,
    fingerprint: BlobId,
}

impl BacktranslationAuditionReceipt {
    pub fn new(
        proposal_fingerprint: BlobId,
        source_work_fingerprint: BlobId,
        controller_call_fingerprint: BlobId,
        cases: Vec<BacktranslationAuditionCase>,
    ) -> Result<Self, BacktranslationError> {
        if cases.len() < MIN_BACKTRANSLATION_AUDITION_CASES {
            return Err(BacktranslationError::TooFewAuditionCases {
                actual: cases.len(),
                minimum: MIN_BACKTRANSLATION_AUDITION_CASES,
            });
        }
        let cases = NonEmptyBoundedVec::new(cases)?;
        let mut case_ids = BTreeSet::new();
        let mut works = BTreeSet::new();
        let mut sources = BTreeSet::new();
        let mut prompts = BTreeSet::new();
        let mut calls = BTreeSet::new();
        let mut evaluator_receipts = BTreeSet::new();
        let writer_binding = cases
            .first()
            .map(|case| {
                (
                    case.writer_model_fingerprint(),
                    case.writer_tokenizer_fingerprint(),
                )
            })
            .ok_or(BacktranslationError::TooFewAuditionCases {
                actual: 0,
                minimum: MIN_BACKTRANSLATION_AUDITION_CASES,
            })?;
        for case in cases.iter() {
            if !case_ids.insert(case.case_id()) {
                return Err(BacktranslationError::DuplicateAuditionCase(case.case_id()));
            }
            if case.work_fingerprint() == source_work_fingerprint {
                return Err(BacktranslationError::AuditionReusesSourceWork);
            }
            if !works.insert(case.work_fingerprint()) {
                return Err(BacktranslationError::AuditionReusesWork(
                    case.work_fingerprint(),
                ));
            }
            let source_key = (case.source().revision_id(), case.source().source_blob_id());
            if !sources.insert(source_key) {
                return Err(BacktranslationError::AuditionReusesExactSource);
            }
            if (
                case.writer_model_fingerprint(),
                case.writer_tokenizer_fingerprint(),
            ) != writer_binding
            {
                return Err(BacktranslationError::AuditionWriterBindingMismatch);
            }
            if !prompts.insert(case.prompt_fingerprint()) {
                return Err(BacktranslationError::AuditionReusesPrompt);
            }
            if case.call_fingerprint() == case.baseline_call_fingerprint() {
                return Err(BacktranslationError::AuditionSelfComparison);
            }
            for call in [case.call_fingerprint(), case.baseline_call_fingerprint()] {
                if !calls.insert(call) {
                    return Err(BacktranslationError::AuditionReusesCall);
                }
            }
            if !evaluator_receipts.insert(case.evaluator_receipt_fingerprint()) {
                return Err(BacktranslationError::AuditionReusesEvaluatorReceipt);
            }
        }
        let mut receipt = Self {
            proposal_fingerprint,
            source_work_fingerprint,
            controller_call_fingerprint,
            cases,
            fingerprint: BlobId::digest(&[]),
        };
        receipt.fingerprint = receipt.compute_fingerprint();
        Ok(receipt)
    }

    pub const fn proposal_fingerprint(&self) -> BlobId {
        self.proposal_fingerprint
    }

    pub const fn source_work_fingerprint(&self) -> BlobId {
        self.source_work_fingerprint
    }

    pub const fn controller_call_fingerprint(&self) -> BlobId {
        self.controller_call_fingerprint
    }

    pub fn cases(&self) -> &[BacktranslationAuditionCase] {
        &self.cases
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(AUDITION_FINGERPRINT_DOMAIN);
        digest.update(self.proposal_fingerprint.as_bytes());
        digest.update(self.source_work_fingerprint.as_bytes());
        digest.update(self.controller_call_fingerprint.as_bytes());
        digest.update((self.cases.len() as u64).to_be_bytes());
        for case in self.cases.iter() {
            case.update_digest(&mut digest);
        }
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktranslationAuditionReceiptWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    proposal_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_work_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    controller_call_fingerprint: BlobId,
    cases: NonEmptyBoundedVec<BacktranslationAuditionCase, MAX_BACKTRANSLATION_AUDITION_CASES>,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for BacktranslationAuditionReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BacktranslationAuditionReceiptWire::deserialize(deserializer)?;
        let receipt = Self::new(
            wire.proposal_fingerprint,
            wire.source_work_fingerprint,
            wire.controller_call_fingerprint,
            wire.cases.into_inner(),
        )
        .map_err(serde::de::Error::custom)?;
        if receipt.fingerprint != wire.fingerprint {
            return Err(serde::de::Error::custom(
                BacktranslationError::AuditionFingerprintMismatch,
            ));
        }
        Ok(receipt)
    }
}

/// A structurally passing proposal/receipt pair.
///
/// This is move-only and cannot be deserialized. It is not backend authorship
/// authority: callers must validate every referenced call, source, output, and
/// evaluator receipt before using its fingerprint in a prompt witness.
pub struct AuditionedBacktranslation {
    proposal: BacktranslationProposal,
    receipt: BacktranslationAuditionReceipt,
    fingerprint: BlobId,
}

impl AuditionedBacktranslation {
    pub const fn proposal(&self) -> &BacktranslationProposal {
        &self.proposal
    }

    pub const fn receipt(&self) -> &BacktranslationAuditionReceipt {
        &self.receipt
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn into_parts(
        self,
    ) -> (
        BacktranslationProposal,
        BacktranslationAuditionReceipt,
        BlobId,
    ) {
        (self.proposal, self.receipt, self.fingerprint)
    }
}

impl std::fmt::Debug for AuditionedBacktranslation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuditionedBacktranslation")
            .field("proposal_fingerprint", &self.proposal.fingerprint())
            .field("receipt_fingerprint", &self.receipt.fingerprint())
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BacktranslationError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("semantic ontology term code must be nonzero")]
    ZeroSemanticTerm,
    #[error("backtranslation field ID must be nonzero")]
    ZeroFieldId,
    #[error("placeholder has kind {actual:?}; expected {expected:?}")]
    WrongPlaceholderKind {
        expected: PlaceholderKind,
        actual: PlaceholderKind,
    },
    #[error("backtranslation evidence repeats an exact source range")]
    DuplicateEvidence,
    #[error("backtranslation field {0} repeats an argument")]
    DuplicateFieldArgument(u32),
    #[error("backtranslation repeats placeholder {0:?}")]
    DuplicatePlaceholder(TypedPlaceholder),
    #[error("backtranslation refers to undeclared placeholder {0:?}")]
    UndeclaredPlaceholder(TypedPlaceholder),
    #[error("backtranslation repeats field ID {0}")]
    DuplicateFieldId(u32),
    #[error("evidence revision/blob does not match the proposal source")]
    EvidenceSourceMismatch,
    #[error("evidence range is not contained by the proposal source range")]
    EvidenceOutsideProposalSource,
    #[error("source revision is {actual}; expected {expected}")]
    SourceRevisionMismatch {
        expected: RevisionId,
        actual: RevisionId,
    },
    #[error("source hashes to {actual}; expected {expected}")]
    SourceBlobMismatch { expected: BlobId, actual: BlobId },
    #[error("backtranslation proposal fingerprint mismatch")]
    ProposalFingerprintMismatch,
    #[error("audition has {actual} cases; minimum is {minimum}")]
    TooFewAuditionCases { actual: usize, minimum: usize },
    #[error("audition repeats case {0}")]
    DuplicateAuditionCase(TrialCaseId),
    #[error("audition reuses the proposal's source work")]
    AuditionReusesSourceWork,
    #[error("audition repeats work {0}")]
    AuditionReusesWork(BlobId),
    #[error("audition repeats an exact source revision/blob")]
    AuditionReusesExactSource,
    #[error("audition cases do not share one exact writer model/tokenizer binding")]
    AuditionWriterBindingMismatch,
    #[error("audition repeats an exact compiled prompt")]
    AuditionReusesPrompt,
    #[error("audition compares one call with itself")]
    AuditionSelfComparison,
    #[error("audition reuses a candidate or baseline call")]
    AuditionReusesCall,
    #[error("audition reuses an evaluator receipt")]
    AuditionReusesEvaluatorReceipt,
    #[error("audition receipt fingerprint mismatch")]
    AuditionFingerprintMismatch,
    #[error("audition receipt names a different proposal")]
    AuditionProposalMismatch,
    #[error("audition receipt names a different source work")]
    AuditionSourceWorkMismatch,
    #[error("audition receipt names a different controller call")]
    AuditionControllerCallMismatch,
    #[error("audition reuses the proposal source revision or bytes")]
    AuditionReusesProposalSource,
    #[error("audition did not improve every case without leakage")]
    AuditionDidNotPass,
}

fn bounded_unique_evidence(
    evidence: Vec<PromptSourceRange>,
) -> Result<
    NonEmptyBoundedVec<PromptSourceRange, MAX_BACKTRANSLATION_EVIDENCE_SPANS>,
    BacktranslationError,
> {
    let evidence = NonEmptyBoundedVec::new(evidence)?;
    let mut unique = BTreeSet::new();
    for span in evidence.iter().copied() {
        let key = (
            span.revision_id(),
            span.source_blob_id(),
            span.range().start(),
            span.range().end(),
        );
        if !unique.insert(key) {
            return Err(BacktranslationError::DuplicateEvidence);
        }
    }
    Ok(evidence)
}

fn validate_evidence_set_in_source(
    source: PromptSourceRange,
    evidence: &[PromptSourceRange],
) -> Result<(), BacktranslationError> {
    for span in evidence.iter().copied() {
        validate_evidence_in_source(source, span)?;
    }
    Ok(())
}

fn validate_evidence_in_source(
    source: PromptSourceRange,
    evidence: PromptSourceRange,
) -> Result<(), BacktranslationError> {
    if evidence.revision_id() != source.revision_id()
        || evidence.source_blob_id() != source.source_blob_id()
    {
        return Err(BacktranslationError::EvidenceSourceMismatch);
    }
    if evidence.range().start() < source.range().start()
        || evidence.range().end() > source.range().end()
    {
        return Err(BacktranslationError::EvidenceOutsideProposalSource);
    }
    Ok(())
}

fn update_source_digest(source: PromptSourceRange, digest: &mut Sha256) {
    digest.update(source.revision_id().as_ulid().to_bytes());
    digest.update(source.source_blob_id().as_bytes());
    digest.update(source.range().start().to_be_bytes());
    digest.update(source.range().end().to_be_bytes());
}

fn update_evidence_digest(evidence: &[PromptSourceRange], digest: &mut Sha256) {
    digest.update((evidence.len() as u64).to_be_bytes());
    for source in evidence.iter().copied() {
        update_source_digest(source, digest);
    }
}

fn fingerprint_accepted(proposal: BlobId, receipt: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(ACCEPTED_FINGERPRINT_DOMAIN);
    digest.update(proposal.as_bytes());
    digest.update(receipt.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
