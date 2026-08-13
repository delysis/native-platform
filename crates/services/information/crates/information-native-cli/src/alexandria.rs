use crate::input::{load_rights, load_use_policy, validate_values};
use crate::{
    AlexandriaBlockArgs, AlexandriaSearchArgs, AlexandriaSourceArgs, CliResult, RegisterSqliteArgs,
    fail, json_value,
};
use information_native_backend_sqlite::{AlexandriaBackend, AlexandriaBackendConfig};
use information_native_retrieval::{LookupRequest, ResourceBackend, RetrievalRouter};
use information_native_store::{ExternalRegistrationRequest, ManagedStore};
use information_native_types::{
    InformationQuery, Provenance, QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId, ReleaseId,
    RepresentationFormat, RepresentationId, ResourceId, RetrievalTarget, RightsStatement,
    UsePolicy,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

pub(crate) fn register_sqlite(args: &RegisterSqliteArgs) -> CliResult<serde_json::Value> {
    if !args.database.is_absolute() {
        return fail("--database must be an absolute path");
    }
    let database = std::fs::canonicalize(&args.database).map_err(|error| {
        crate::message_error(format!(
            "cannot resolve external database {}: {error}",
            args.database.display()
        ))
    })?;
    validate_values("provenance source inputs", &args.source_input, 1_024, 512)?;
    let rights = load_rights(args.rights_json.as_deref())?;
    let use_policy = coherent_use_policy(&rights, args.use_policy_json.as_deref())?;
    let source_uri = match &args.source_uri {
        Some(value) => validate_provenance_uri(value)?,
        None => Url::from_file_path(&database)
            .map_err(|()| crate::message_error("--database cannot be represented as a file URI"))?
            .to_string(),
    };
    let request = ExternalRegistrationRequest {
        installation_id: information_native_types::InstallationId::parse(
            args.installation_id.clone(),
        )?,
        resource_id: ResourceId::parse(args.resource_id.clone())?,
        release_id: ReleaseId::parse(args.release_id.clone())?,
        representation_id: RepresentationId::parse(args.representation_id.clone())?,
        format: RepresentationFormat {
            kind: args.format.into(),
            profile: args.profile.clone(),
            media_type: args.media_type.clone(),
        },
        absolute_path: database,
        access_mode: args.access_mode.into(),
        provenance: Provenance {
            publisher: args.publisher.clone(),
            source_uri,
            upstream_record_id: args.upstream_record_id.clone(),
            source_inputs: args.source_input.clone(),
            transformation: args.transformation.clone(),
            metadata: BTreeMap::new(),
        },
        rights,
        use_policy,
    };
    let store = ManagedStore::open(&args.root)?;
    json_value(&store.register_external(&request)?)
}

pub(crate) fn search(args: &AlexandriaSearchArgs) -> CliResult<serde_json::Value> {
    validate_values("languages", &args.languages, 1_024, 512)?;
    validate_values("subjects", &args.subjects, 1_024, 512)?;
    validate_values("document ids", &args.document_ids, 1_024, 512)?;
    let mounted = mount(&args.source)?;
    let mut fields = BTreeMap::new();
    for field in &args.fields {
        if fields
            .insert(field.key.clone(), field.value.clone())
            .is_some()
        {
            return fail(format!(
                "query field {:?} was supplied more than once",
                field.key
            ));
        }
    }
    let query_id = match &args.query_id {
        Some(value) => QueryId::parse(value.clone())?,
        None => QueryId::new(),
    };
    let query = InformationQuery {
        schema: QUERY_SCHEMA.to_string(),
        query_id,
        text: args.text.clone(),
        syntax: args.syntax.into(),
        purpose: args.purpose.into(),
        targets: vec![RetrievalTarget {
            resource_id: mounted.resource_id.clone(),
            release_id: mounted.release_id.clone(),
            representation_id: mounted.representation_id.clone(),
        }],
        resources: vec![mounted.resource_id.clone()],
        representations: vec![mounted.representation_id.clone()],
        filters: QueryFilters {
            languages: args.languages.clone(),
            subjects: args.subjects.clone(),
            document_ids: args.document_ids.clone(),
            spatial: None,
            temporal_start: None,
            temporal_end: None,
            fields,
        },
        budget: QueryBudget {
            max_hits: args.max_hits,
            max_hits_per_backend: args.max_hits,
            max_backends: 1,
            max_context_chars: args.max_context_chars,
            timeout_ms: args.timeout_ms,
        },
    };
    json_value(&mounted.router.search(&query)?)
}

pub(crate) fn block(args: &AlexandriaBlockArgs) -> CliResult<serde_json::Value> {
    let mounted = mount(&args.source)?;
    let request = LookupRequest {
        resource_id: mounted.resource_id,
        release_id: mounted.release_id,
        representation_id: mounted.representation_id,
        purpose: args.purpose.into(),
        collection: Some("blocks".to_string()),
        key: args.block_id.clone(),
        max_context_chars: args.max_context_chars,
        timeout_ms: args.timeout_ms,
    };
    json_value(&mounted.router.lookup(&request)?)
}

struct MountedAlexandria {
    router: RetrievalRouter,
    resource_id: ResourceId,
    release_id: ReleaseId,
    representation_id: RepresentationId,
}

fn mount(args: &AlexandriaSourceArgs) -> CliResult<MountedAlexandria> {
    let resource_id = ResourceId::parse(args.resource_id.clone())?;
    let release_id = ReleaseId::parse(args.release_id.clone())?;
    let representation_id = RepresentationId::parse(args.representation_id.clone())?;
    let mut config = AlexandriaBackendConfig::new(
        args.backend_id.clone(),
        args.label.clone(),
        resource_id.clone(),
        release_id.clone(),
        representation_id.clone(),
        &args.database,
        args.access_mode.into(),
        args.publisher.clone(),
    );
    config.verified_source_sha256 = args.verified_sha256.clone();
    config.rights = load_rights(args.rights_json.as_deref())?;
    config.use_policy = coherent_use_policy(&config.rights, args.use_policy_json.as_deref())?;
    config.context_radius = args.context_radius;
    config.max_snippet_chars = args.max_snippet_chars;
    let backend: Arc<dyn ResourceBackend> = Arc::new(AlexandriaBackend::open(config)?);
    let router = RetrievalRouter::from_backends([backend])?;
    Ok(MountedAlexandria {
        router,
        resource_id,
        release_id,
        representation_id,
    })
}

fn coherent_use_policy(
    rights: &[RightsStatement],
    path: Option<&std::path::Path>,
) -> CliResult<UsePolicy> {
    let mut policy = load_use_policy(path)?;
    if path.is_none() && rights.is_empty() {
        policy.attribution_required = false;
    }
    policy.validate_with_rights(rights)?;
    Ok(policy)
}

fn validate_provenance_uri(value: &str) -> CliResult<String> {
    let uri = Url::parse(value)
        .map_err(|_| crate::message_error(format!("invalid provenance source URI: {value:?}")))?;
    if !uri.username().is_empty() || uri.password().is_some() {
        return fail("credentials embedded in provenance source URIs are forbidden");
    }
    Ok(uri.to_string())
}
