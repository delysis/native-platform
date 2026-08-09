use crate::input::{load_rights, load_use_policy, validate_values};
use crate::{
    CliResult, CommunityMessageArgs, CommunitySearchArgs, CommunitySourceArgs, fail, json_value,
};
use information_native_backend_community::{
    CommunityArchiveBackend, CommunityArchiveBackendConfig,
};
use information_native_retrieval::{LookupRequest, ResourceBackend, RetrievalRouter};
use information_native_types::{
    InformationQuery, QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId, ReleaseId,
    RepresentationId, ResourceId, RetrievalTarget,
};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn search(args: &CommunitySearchArgs) -> CliResult<serde_json::Value> {
    validate_values("document ids", &args.document_ids, 256, 8_192)?;
    let fields = compiled_fields(&args.fields, 16)?;
    let mounted = mount(&args.source)?;
    let query = InformationQuery {
        schema: QUERY_SCHEMA.to_string(),
        query_id: query_id(args.query_id.as_ref())?,
        text: args.text.clone(),
        syntax: args.syntax.into(),
        purpose: args.purpose.into(),
        targets: vec![mounted.target()],
        resources: vec![mounted.resource_id.clone()],
        representations: vec![mounted.representation_id.clone()],
        filters: QueryFilters {
            languages: Vec::new(),
            subjects: Vec::new(),
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

pub(crate) fn message(args: &CommunityMessageArgs) -> CliResult<serde_json::Value> {
    let mounted = mount(&args.source)?;
    let request = LookupRequest {
        resource_id: mounted.resource_id,
        release_id: mounted.release_id,
        representation_id: mounted.representation_id,
        purpose: args.purpose.into(),
        collection: Some("messages".to_string()),
        key: args.message_key.clone(),
        max_context_chars: args.max_context_chars,
        timeout_ms: args.timeout_ms,
    };
    json_value(&mounted.router.lookup(&request)?)
}

struct MountedCommunity {
    router: RetrievalRouter,
    resource_id: ResourceId,
    release_id: ReleaseId,
    representation_id: RepresentationId,
}

impl MountedCommunity {
    fn target(&self) -> RetrievalTarget {
        RetrievalTarget {
            resource_id: self.resource_id.clone(),
            release_id: self.release_id.clone(),
            representation_id: self.representation_id.clone(),
        }
    }
}

fn mount(args: &CommunitySourceArgs) -> CliResult<MountedCommunity> {
    let resource_id = ResourceId::parse(args.resource_id.clone())?;
    let release_id = ReleaseId::parse(args.release_id.clone())?;
    let representation_id = RepresentationId::parse(args.representation_id.clone())?;
    let mut config = CommunityArchiveBackendConfig::new(
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
    if args.rights_json.is_some() {
        config.rights = load_rights(args.rights_json.as_deref())?;
    }
    if args.use_policy_json.is_some() {
        config.use_policy = load_use_policy(args.use_policy_json.as_deref())?;
    }
    config.allow_private_model_context = args.allow_private_model_context;
    config.max_snippet_chars = args.max_snippet_chars;
    let backend: Arc<dyn ResourceBackend> = Arc::new(CommunityArchiveBackend::open(config)?);
    let router = RetrievalRouter::from_backends([backend])?;
    Ok(MountedCommunity {
        router,
        resource_id,
        release_id,
        representation_id,
    })
}

fn query_id(value: Option<&String>) -> CliResult<QueryId> {
    match value {
        Some(value) => Ok(QueryId::parse(value.clone())?),
        None => Ok(QueryId::new()),
    }
}

fn compiled_fields(
    values: &[crate::KeyValueArg],
    max_fields: usize,
) -> CliResult<BTreeMap<String, String>> {
    if values.len() > max_fields {
        return fail(format!(
            "query has {} field filters, exceeding the {max_fields}-filter limit",
            values.len()
        ));
    }
    let mut fields = BTreeMap::new();
    for field in values {
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
    Ok(fields)
}
