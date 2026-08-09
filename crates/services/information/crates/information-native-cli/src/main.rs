#![forbid(unsafe_code)]

mod alexandria;
mod community;
mod encyclopedia;
mod input;
mod providers;
mod scripture;

use chrono::Utc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use information_native_acquire::{AcquireClient, AcquireConfig, AcquisitionPolicy, NetworkScope};
use information_native_backend_sqlite::{AlexandriaBackend, AlexandriaBackendConfig};
use information_native_catalog::{
    CatalogIndex, CatalogSearchQuery, PlanRequest, SearchTextMode, SourceRegistry,
    SourceRegistrySupport, parse_source_registry_with_limit,
};
use information_native_host::{HOST_VERSION, InformationHost};
use information_native_retrieval::{ResourceBackend, RetrievalRouter};
use information_native_store::{ManagedStore, PartialInstallState};
use information_native_types::{
    ArtifactId, CatalogTrust, ExternalAccessMode, FormatKind, InformationCapability,
    InformationCatalog, InstallPlan, InstallSelection, InstallationId, Publisher, QuerySyntax,
    ReleaseId, RepresentationId, ResourceId, ResourceKind, ResourceRecord, RetrievalPurpose,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CATALOG_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_DISCOVERY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DISCOVERY_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_SOURCE_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_SOURCE_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_METALINK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PLAN_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PROVIDER_PAGES: usize = 100;
const MAX_PROVIDER_DOCUMENTS: usize = 64;
const MAX_PROVIDER_RECORDS: usize = 10_000;
const KIWIX_OPDS_URI: &str = "https://opds.library.kiwix.org/catalog/v2/entries";
const OVERTURE_STAC_URI: &str = "https://stac.overturemaps.org/catalog.json";

pub(crate) type CliResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(
    name = "information-native",
    version = HOST_VERSION,
    about = "Inspect, discover, install, register, and query offline information resources",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check acquisition construction and optional catalog, store, and Alexandria inputs.
    Doctor(DoctorArgs),
    /// Report catalog, managed-store, and mounted-backend status.
    Status(StatusArgs),
    /// Validate, search, or resolve an exact plan from a normalized catalog file.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Validate or list an operator-supplied upstream source registry.
    Sources {
        #[command(subcommand)]
        command: SourcesCommand,
    },
    /// Perform bounded live discovery against an upstream provider document.
    Discover {
        #[command(subcommand)]
        command: DiscoveryCommand,
    },
    /// Fetch, verify, and atomically activate an exact install-plan file.
    Install(InstallArgs),
    /// Register an external SQLite/Alexandria file without modifying it.
    RegisterSqlite(RegisterSqliteArgs),
    /// List ready managed installs, external registrations, and partial state.
    Installed(InstalledArgs),
    /// Rehash every artifact in one ready managed installation.
    Verify(VerifyArgs),
    /// Print the append-only receipt history for one managed installation.
    ReceiptHistory(ReceiptHistoryArgs),
    /// Query an Alexandria database through the strict read-only backend.
    Alexandria {
        #[command(subcommand)]
        command: AlexandriaCommand,
    },
    /// Query a Community Archive v28 database through its private, read-only profile.
    CommunityArchive {
        #[command(subcommand)]
        command: CommunityCommand,
    },
    /// Query the compiled local encyclopedia article profile read-only.
    Encyclopedia {
        #[command(subcommand)]
        command: EncyclopediaCommand,
    },
    /// Query Alexandria's Scripture-reference sidecar read-only.
    Scripture {
        #[command(subcommand)]
        command: ScriptureCommand,
    },
    /// Print the model-neutral JSON definitions for offline-information tools.
    AgentTools,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Optional normalized catalog to validate.
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// Optional managed root to open and inspect. A missing root is initialized.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Optional Alexandria SQLite database whose schema should be checked read-only.
    #[arg(long)]
    alexandria: Option<PathBuf>,
    /// Access policy used by the optional Alexandria probe.
    #[arg(long, value_enum, default_value = "live-read-only")]
    access_mode: CliAccessMode,
    #[arg(
        long,
        default_value_t = DEFAULT_CATALOG_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CATALOG_BYTES)
    )]
    max_catalog_bytes: u64,
    #[command(flatten)]
    transport: TransportArgs,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[arg(long)]
    catalog: PathBuf,
    #[arg(long)]
    root: PathBuf,
    #[arg(
        long,
        default_value_t = DEFAULT_CATALOG_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CATALOG_BYTES)
    )]
    max_catalog_bytes: u64,
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    Validate(CatalogFileArgs),
    Search(CatalogSearchArgs),
    Plan(CatalogPlanArgs),
}

#[derive(Debug, Subcommand)]
enum SourcesCommand {
    /// Validate the strict v1 source-registry contract without network access.
    Validate(SourceRegistryFileArgs),
    /// List validated source entries and their honest shipped-support level.
    List(SourceRegistryFileArgs),
}

#[derive(Debug, Args)]
struct SourceRegistryFileArgs {
    /// Source-registry JSON path. This is explicit because working directories
    /// and installed-package layouts cannot reliably locate the repository copy.
    #[arg(long)]
    registry: PathBuf,
    #[arg(
        long,
        default_value_t = DEFAULT_SOURCE_REGISTRY_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_SOURCE_REGISTRY_BYTES)
    )]
    max_bytes: u64,
}

#[derive(Debug, Args)]
struct CatalogFileArgs {
    #[arg(long)]
    catalog: PathBuf,
    #[arg(
        long,
        default_value_t = DEFAULT_CATALOG_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CATALOG_BYTES)
    )]
    max_catalog_bytes: u64,
}

#[derive(Debug, Args)]
struct CatalogSearchArgs {
    #[arg(long)]
    catalog: PathBuf,
    #[arg(long, value_parser = parse_search_text)]
    text: String,
    #[arg(long, value_enum, default_value = "all-terms")]
    mode: CatalogTextMode,
    #[arg(long, value_enum)]
    kind: Vec<CliResourceKind>,
    #[arg(long)]
    language: Vec<String>,
    #[arg(long)]
    subject: Vec<String>,
    #[arg(long, value_enum)]
    format: Vec<CliFormatKind>,
    #[arg(long, value_enum)]
    capability: Vec<CliCapability>,
    #[arg(
        long,
        default_value_t = 100,
        value_parser = parse_catalog_limit
    )]
    limit: usize,
    #[arg(
        long,
        default_value_t = DEFAULT_CATALOG_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CATALOG_BYTES)
    )]
    max_catalog_bytes: u64,
}

#[derive(Debug, Args)]
struct CatalogPlanArgs {
    #[arg(long)]
    catalog: PathBuf,
    /// Defaults to a newly generated UUID when omitted.
    #[arg(long)]
    installation_id: Option<String>,
    #[arg(long)]
    resource_id: String,
    #[arg(long)]
    release_id: String,
    #[arg(long)]
    representation_id: String,
    /// Exact override in ARTIFACT_ID=DECLARED_URI form. Repeatable.
    #[arg(long, value_parser = parse_key_value)]
    mirror: Vec<KeyValueArg>,
    /// Free bytes observed by the caller; plans fail if this is insufficient.
    #[arg(long)]
    available_bytes: Option<u64>,
    #[arg(
        long,
        default_value_t = DEFAULT_CATALOG_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CATALOG_BYTES)
    )]
    max_catalog_bytes: u64,
}

#[derive(Debug, Subcommand)]
enum DiscoveryCommand {
    /// Traverse bounded Kiwix OPDS `next` pages.
    KiwixOpds(KiwixDiscoveryArgs),
    /// Traverse bounded Overture/STAC catalogs.
    OvertureStac(StacDiscoveryArgs),
    /// Resolve one bounded Metalink 4 document into exact payload metadata.
    Metalink(MetalinkDiscoveryArgs),
}

#[derive(Debug, Args)]
pub(crate) struct KiwixDiscoveryArgs {
    #[arg(long, default_value = KIWIX_OPDS_URI)]
    pub(crate) source_uri: String,
    #[arg(
        long,
        default_value_t = 4,
        value_parser = parse_provider_pages
    )]
    pub(crate) max_pages: usize,
    #[arg(
        long,
        default_value_t = 1_000,
        value_parser = parse_provider_records
    )]
    pub(crate) max_records: usize,
    #[arg(
        long,
        default_value_t = DEFAULT_DISCOVERY_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_DISCOVERY_BYTES)
    )]
    pub(crate) max_total_bytes: u64,
    #[command(flatten)]
    pub(crate) transport: TransportArgs,
}

#[derive(Debug, Args)]
pub(crate) struct StacDiscoveryArgs {
    #[arg(long, default_value = OVERTURE_STAC_URI)]
    pub(crate) source_uri: String,
    #[arg(long, value_enum, default_value = "latest-chain")]
    pub(crate) traversal: StacTraversal,
    #[arg(
        long,
        default_value_t = 8,
        value_parser = parse_provider_documents
    )]
    pub(crate) max_documents: usize,
    #[arg(
        long,
        default_value_t = 1_000,
        value_parser = parse_provider_records
    )]
    pub(crate) max_records: usize,
    #[arg(
        long,
        default_value_t = DEFAULT_DISCOVERY_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_DISCOVERY_BYTES)
    )]
    pub(crate) max_total_bytes: u64,
    #[command(flatten)]
    pub(crate) transport: TransportArgs,
}

#[derive(Debug, Args)]
pub(crate) struct MetalinkDiscoveryArgs {
    #[arg(long)]
    pub(crate) source_uri: String,
    #[arg(
        long,
        default_value_t = DEFAULT_METALINK_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_DISCOVERY_BYTES)
    )]
    pub(crate) max_bytes: u64,
    #[command(flatten)]
    pub(crate) transport: TransportArgs,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct TransportArgs {
    #[arg(
        long,
        default_value_t = 15,
        value_parser = clap::value_parser!(u64).range(1..=300)
    )]
    connect_timeout_seconds: u64,
    #[arg(
        long,
        default_value_t = 1_800,
        value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    request_timeout_seconds: u64,
    #[arg(
        long,
        default_value_t = 5,
        value_parser = parse_redirect_limit
    )]
    max_redirects: usize,
    /// Grant acquisition access to file: URIs beneath this canonical directory.
    #[arg(long = "allow-file-root", value_name = "DIRECTORY")]
    allow_file_roots: Vec<PathBuf>,
    /// Permit loopback, link-local, and private network destinations.
    #[arg(long)]
    allow_private_network: bool,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    root: PathBuf,
    #[arg(
        long,
        default_value_t = MAX_PLAN_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=MAX_PLAN_BYTES)
    )]
    max_plan_bytes: u64,
    #[command(flatten)]
    transport: TransportArgs,
}

#[derive(Debug, Args)]
pub(crate) struct RegisterSqliteArgs {
    #[arg(long)]
    pub(crate) root: PathBuf,
    /// Must be an absolute path outside the managed root.
    #[arg(long)]
    pub(crate) database: PathBuf,
    #[arg(long)]
    pub(crate) installation_id: String,
    #[arg(long)]
    pub(crate) resource_id: String,
    #[arg(long)]
    pub(crate) release_id: String,
    #[arg(long)]
    pub(crate) representation_id: String,
    #[arg(long, value_enum, default_value = "alexandria-sqlite")]
    pub(crate) format: CliSqliteFormat,
    #[arg(long, default_value = "alexandria.blocks.v1")]
    pub(crate) profile: Option<String>,
    #[arg(long, default_value = "application/vnd.sqlite3")]
    pub(crate) media_type: Option<String>,
    #[arg(long, value_enum, default_value = "live-read-only")]
    pub(crate) access_mode: CliAccessMode,
    /// Publisher or custodian responsible for this external source.
    #[arg(long, value_parser = parse_nonempty_text)]
    pub(crate) publisher: String,
    /// Provenance source URI. Defaults to the database's absolute file URI.
    #[arg(long)]
    pub(crate) source_uri: Option<String>,
    #[arg(long)]
    pub(crate) upstream_record_id: Option<String>,
    #[arg(long)]
    pub(crate) source_input: Vec<String>,
    #[arg(long)]
    pub(crate) transformation: Option<String>,
    /// JSON array of RightsStatement objects, limited to 1 MiB.
    #[arg(long)]
    pub(crate) rights_json: Option<PathBuf>,
    /// JSON UsePolicy object, limited to 1 MiB.
    #[arg(long)]
    pub(crate) use_policy_json: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InstalledArgs {
    #[arg(long)]
    root: PathBuf,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    installation_id: String,
}

#[derive(Debug, Args)]
struct ReceiptHistoryArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    installation_id: String,
}

#[derive(Debug, Subcommand)]
enum AlexandriaCommand {
    Search(AlexandriaSearchArgs),
    Block(AlexandriaBlockArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AlexandriaSourceArgs {
    #[arg(long)]
    pub(crate) database: PathBuf,
    #[arg(long)]
    pub(crate) resource_id: String,
    #[arg(long)]
    pub(crate) release_id: String,
    #[arg(long)]
    pub(crate) representation_id: String,
    #[arg(long, default_value = "alexandria-cli")]
    pub(crate) backend_id: String,
    #[arg(long, default_value = "Alexandria")]
    pub(crate) label: String,
    #[arg(long, value_parser = parse_nonempty_text)]
    pub(crate) publisher: String,
    #[arg(long, value_enum, default_value = "live-read-only")]
    pub(crate) access_mode: CliAccessMode,
    #[arg(long, value_parser = parse_sha256)]
    pub(crate) verified_sha256: Option<String>,
    #[arg(long)]
    pub(crate) rights_json: Option<PathBuf>,
    #[arg(long)]
    pub(crate) use_policy_json: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u8).range(0..=8)
    )]
    pub(crate) context_radius: u8,
    #[arg(
        long,
        default_value_t = 700,
        value_parser = clap::value_parser!(u32).range(1..=4_096)
    )]
    pub(crate) max_snippet_chars: u32,
}

#[derive(Debug, Args)]
pub(crate) struct AlexandriaSearchArgs {
    #[command(flatten)]
    pub(crate) source: AlexandriaSourceArgs,
    #[arg(long, value_parser = parse_search_text)]
    pub(crate) text: String,
    #[arg(long, value_enum, default_value = "natural-terms")]
    pub(crate) syntax: CliQuerySyntax,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(long)]
    pub(crate) query_id: Option<String>,
    #[arg(long)]
    pub(crate) languages: Vec<String>,
    #[arg(long)]
    pub(crate) subjects: Vec<String>,
    #[arg(long)]
    pub(crate) document_ids: Vec<String>,
    /// Compiled backend filter in KEY=VALUE form. Repeatable.
    #[arg(long, value_parser = parse_key_value)]
    pub(crate) fields: Vec<KeyValueArg>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u32).range(1..=1_000)
    )]
    pub(crate) max_hits: u32,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Args)]
pub(crate) struct AlexandriaBlockArgs {
    #[command(flatten)]
    pub(crate) source: AlexandriaSourceArgs,
    #[arg(long, value_parser = parse_lookup_key)]
    pub(crate) block_id: String,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Subcommand)]
enum CommunityCommand {
    /// Search message text and compiled Community Archive metadata fields.
    Search(CommunitySearchArgs),
    /// Look up one message by rowid:, tweet_id:, note_id:, or a returned compound key.
    Message(CommunityMessageArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct CommunitySourceArgs {
    #[arg(long)]
    pub(crate) database: PathBuf,
    #[arg(long)]
    pub(crate) resource_id: String,
    #[arg(long)]
    pub(crate) release_id: String,
    #[arg(long)]
    pub(crate) representation_id: String,
    #[arg(
        long,
        default_value = "community-archive-cli",
        value_parser = parse_nonempty_text
    )]
    pub(crate) backend_id: String,
    #[arg(
        long,
        default_value = "Community Archive",
        value_parser = parse_nonempty_text
    )]
    pub(crate) label: String,
    #[arg(long, value_parser = parse_nonempty_text)]
    pub(crate) publisher: String,
    #[arg(long, value_enum, default_value = "live-read-only")]
    pub(crate) access_mode: CliAccessMode,
    #[arg(long, value_parser = parse_sha256)]
    pub(crate) verified_sha256: Option<String>,
    /// Optional bounded JSON array of source-wide RightsStatement objects.
    #[arg(long)]
    pub(crate) rights_json: Option<PathBuf>,
    /// Optional bounded JSON UsePolicy. Model-context retrieval must be explicitly allowed here.
    #[arg(long)]
    pub(crate) use_policy_json: Option<PathBuf>,
    /// Permit private records in model-context results. Without this, they are omitted or denied.
    #[arg(long)]
    pub(crate) allow_private_model_context: bool,
    #[arg(
        long,
        default_value_t = 900,
        value_parser = clap::value_parser!(u32).range(1..=4_096)
    )]
    pub(crate) max_snippet_chars: u32,
}

#[derive(Debug, Args)]
pub(crate) struct CommunitySearchArgs {
    #[command(flatten)]
    pub(crate) source: CommunitySourceArgs,
    #[arg(long, value_parser = parse_search_text)]
    pub(crate) text: String,
    #[arg(long, value_enum, default_value = "natural-terms")]
    pub(crate) syntax: CliQuerySyntax,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(long)]
    pub(crate) query_id: Option<String>,
    /// rowid:, tweet_id:, or note_id: key. Repeatable, capped by the backend.
    #[arg(long = "document-id")]
    pub(crate) document_ids: Vec<String>,
    /// Compiled filter in KEY=VALUE form: kind, source_collection, username, note_id, tweet_id.
    #[arg(long, value_parser = parse_key_value)]
    pub(crate) fields: Vec<KeyValueArg>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u32).range(1..=1_000)
    )]
    pub(crate) max_hits: u32,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Args)]
pub(crate) struct CommunityMessageArgs {
    #[command(flatten)]
    pub(crate) source: CommunitySourceArgs,
    #[arg(long, value_parser = parse_lookup_key)]
    pub(crate) message_key: String,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Subcommand)]
enum EncyclopediaCommand {
    /// Search compiled article text and metadata fields.
    Search(EncyclopediaSearchArgs),
    /// Look up one article by its positive numeric id.
    Article(EncyclopediaArticleArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EncyclopediaSourceArgs {
    #[arg(long)]
    pub(crate) database: PathBuf,
    #[arg(long)]
    pub(crate) resource_id: String,
    #[arg(long)]
    pub(crate) release_id: String,
    #[arg(long)]
    pub(crate) representation_id: String,
    #[arg(
        long,
        default_value = "encyclopedia-cli",
        value_parser = parse_nonempty_text
    )]
    pub(crate) backend_id: String,
    #[arg(
        long,
        default_value = "Encyclopedia",
        value_parser = parse_nonempty_text
    )]
    pub(crate) label: String,
    #[arg(long, value_parser = parse_nonempty_text)]
    pub(crate) publisher: String,
    #[arg(long, value_enum, default_value = "live-read-only")]
    pub(crate) access_mode: CliAccessMode,
    #[arg(long, value_parser = parse_sha256)]
    pub(crate) verified_sha256: Option<String>,
    /// Optional bounded JSON array of source-wide RightsStatement objects.
    #[arg(long)]
    pub(crate) rights_json: Option<PathBuf>,
    /// Optional bounded JSON UsePolicy. The conservative backend default is retained when absent.
    #[arg(long)]
    pub(crate) use_policy_json: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = 700,
        value_parser = clap::value_parser!(u32).range(1..=4_096)
    )]
    pub(crate) max_snippet_chars: u32,
}

#[derive(Debug, Args)]
pub(crate) struct EncyclopediaSearchArgs {
    #[command(flatten)]
    pub(crate) source: EncyclopediaSourceArgs,
    #[arg(long, value_parser = parse_search_text)]
    pub(crate) text: String,
    #[arg(long, value_enum, default_value = "natural-terms")]
    pub(crate) syntax: CliQuerySyntax,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(long)]
    pub(crate) query_id: Option<String>,
    /// Exact article category values. Repeatable.
    #[arg(long = "subject")]
    pub(crate) subjects: Vec<String>,
    /// Positive numeric article ids. Repeatable.
    #[arg(long = "document-id")]
    pub(crate) document_ids: Vec<String>,
    /// Compiled KEY=VALUE filter: authors, category, edition, headword, origin,
    /// revision, source_container, or title.
    #[arg(long, value_parser = parse_key_value)]
    pub(crate) fields: Vec<KeyValueArg>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u32).range(1..=1_000)
    )]
    pub(crate) max_hits: u32,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Args)]
pub(crate) struct EncyclopediaArticleArgs {
    #[command(flatten)]
    pub(crate) source: EncyclopediaSourceArgs,
    #[arg(long, value_parser = parse_positive_record_id)]
    pub(crate) article_id: String,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Subcommand)]
enum ScriptureCommand {
    /// Search normalized Scripture references using bounded prefix or exact matching.
    Search(ScriptureSearchArgs),
    /// Look up one citation occurrence by its positive numeric id.
    Occurrence(ScriptureOccurrenceArgs),
    /// Look up one normalized passage by its positive numeric id.
    Passage(ScripturePassageArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ScriptureSourceArgs {
    #[arg(long)]
    pub(crate) database: PathBuf,
    #[arg(long)]
    pub(crate) resource_id: String,
    #[arg(long)]
    pub(crate) release_id: String,
    #[arg(long)]
    pub(crate) representation_id: String,
    #[arg(
        long,
        default_value = "scripture-cli",
        value_parser = parse_nonempty_text
    )]
    pub(crate) backend_id: String,
    #[arg(
        long,
        default_value = "Scripture Reference Index",
        value_parser = parse_nonempty_text
    )]
    pub(crate) label: String,
    #[arg(long, value_parser = parse_nonempty_text)]
    pub(crate) publisher: String,
    #[arg(long, value_enum, default_value = "live-read-only")]
    pub(crate) access_mode: CliAccessMode,
    #[arg(long, value_parser = parse_sha256)]
    pub(crate) verified_sha256: Option<String>,
    /// Optional bounded JSON array replacing the backend's unknown-rights statement.
    #[arg(long)]
    pub(crate) rights_json: Option<PathBuf>,
    /// Optional bounded JSON UsePolicy. The local-UI-only default is retained when absent.
    #[arg(long)]
    pub(crate) use_policy_json: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = 512,
        value_parser = clap::value_parser!(u32).range(1..=4_096)
    )]
    pub(crate) max_snippet_chars: u32,
}

#[derive(Debug, Args)]
pub(crate) struct ScriptureSearchArgs {
    #[command(flatten)]
    pub(crate) source: ScriptureSourceArgs,
    #[arg(long, value_parser = parse_scripture_reference)]
    pub(crate) reference: String,
    #[arg(long, value_enum, default_value = "prefix")]
    pub(crate) syntax: CliScriptureQuerySyntax,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(long)]
    pub(crate) query_id: Option<String>,
    /// Alexandria document ids used to constrain citation occurrences. Repeatable.
    #[arg(long = "document-id")]
    pub(crate) document_ids: Vec<String>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u32).range(1..=1_000)
    )]
    pub(crate) max_hits: u32,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Args)]
pub(crate) struct ScriptureOccurrenceArgs {
    #[command(flatten)]
    pub(crate) source: ScriptureSourceArgs,
    #[arg(long, value_parser = parse_positive_record_id)]
    pub(crate) occurrence_id: String,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Args)]
pub(crate) struct ScripturePassageArgs {
    #[command(flatten)]
    pub(crate) source: ScriptureSourceArgs,
    #[arg(long, value_parser = parse_positive_record_id)]
    pub(crate) passage_id: String,
    #[arg(long, value_enum, default_value = "local-ui")]
    pub(crate) purpose: CliRetrievalPurpose,
    #[arg(
        long,
        default_value_t = 24_000,
        value_parser = clap::value_parser!(u32).range(1..=2_000_000)
    )]
    pub(crate) max_context_chars: u32,
    #[arg(
        long,
        default_value_t = 10_000,
        value_parser = clap::value_parser!(u64).range(1..=300_000)
    )]
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct KeyValueArg {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CatalogTextMode {
    AllTerms,
    AnyTerm,
}

impl From<CatalogTextMode> for SearchTextMode {
    fn from(value: CatalogTextMode) -> Self {
        match value {
            CatalogTextMode::AllTerms => Self::AllTerms,
            CatalogTextMode::AnyTerm => Self::AnyTerm,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliResourceKind {
    Encyclopedia,
    TextCorpus,
    PublicationCollection,
    KnowledgeGraph,
    Map,
    Gazetteer,
    Bibliography,
    MediaArchive,
    WebArchive,
    StructuredDataset,
    Mixed,
}

impl From<CliResourceKind> for ResourceKind {
    fn from(value: CliResourceKind) -> Self {
        match value {
            CliResourceKind::Encyclopedia => Self::Encyclopedia,
            CliResourceKind::TextCorpus => Self::TextCorpus,
            CliResourceKind::PublicationCollection => Self::PublicationCollection,
            CliResourceKind::KnowledgeGraph => Self::KnowledgeGraph,
            CliResourceKind::Map => Self::Map,
            CliResourceKind::Gazetteer => Self::Gazetteer,
            CliResourceKind::Bibliography => Self::Bibliography,
            CliResourceKind::MediaArchive => Self::MediaArchive,
            CliResourceKind::WebArchive => Self::WebArchive,
            CliResourceKind::StructuredDataset => Self::StructuredDataset,
            CliResourceKind::Mixed => Self::Mixed,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFormatKind {
    AlexandriaSqlite,
    SqliteFts5,
    Zim,
    OsmPbf,
    GeoParquet,
    PmTiles,
    FlatGeobuf,
    GeoJsonSequence,
    GeoPackage,
    Parquet,
    JsonLines,
    Csv,
    Epub,
    Warc,
    Hdt,
    Directory,
}

impl From<CliFormatKind> for FormatKind {
    fn from(value: CliFormatKind) -> Self {
        match value {
            CliFormatKind::AlexandriaSqlite => Self::AlexandriaSqlite,
            CliFormatKind::SqliteFts5 => Self::SqliteFts5,
            CliFormatKind::Zim => Self::Zim,
            CliFormatKind::OsmPbf => Self::OsmPbf,
            CliFormatKind::GeoParquet => Self::GeoParquet,
            CliFormatKind::PmTiles => Self::PmTiles,
            CliFormatKind::FlatGeobuf => Self::FlatGeobuf,
            CliFormatKind::GeoJsonSequence => Self::GeoJsonSequence,
            CliFormatKind::GeoPackage => Self::GeoPackage,
            CliFormatKind::Parquet => Self::Parquet,
            CliFormatKind::JsonLines => Self::JsonLines,
            CliFormatKind::Csv => Self::Csv,
            CliFormatKind::Epub => Self::Epub,
            CliFormatKind::Warc => Self::Warc,
            CliFormatKind::Hdt => Self::Hdt,
            CliFormatKind::Directory => Self::Directory,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliCapability {
    LexicalSearch,
    ArticleRead,
    RecordLookup,
    SpatialFilter,
    TemporalFilter,
    GraphTraversal,
    TileRead,
    MediaRead,
    RandomAccess,
}

impl From<CliCapability> for InformationCapability {
    fn from(value: CliCapability) -> Self {
        match value {
            CliCapability::LexicalSearch => Self::LexicalSearch,
            CliCapability::ArticleRead => Self::ArticleRead,
            CliCapability::RecordLookup => Self::RecordLookup,
            CliCapability::SpatialFilter => Self::SpatialFilter,
            CliCapability::TemporalFilter => Self::TemporalFilter,
            CliCapability::GraphTraversal => Self::GraphTraversal,
            CliCapability::TileRead => Self::TileRead,
            CliCapability::MediaRead => Self::MediaRead,
            CliCapability::RandomAccess => Self::RandomAccess,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliSqliteFormat {
    AlexandriaSqlite,
    SqliteFts5,
}

impl From<CliSqliteFormat> for FormatKind {
    fn from(value: CliSqliteFormat) -> Self {
        match value {
            CliSqliteFormat::AlexandriaSqlite => Self::AlexandriaSqlite,
            CliSqliteFormat::SqliteFts5 => Self::SqliteFts5,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliAccessMode {
    LiveReadOnly,
    ImmutableReadOnly,
}

impl From<CliAccessMode> for ExternalAccessMode {
    fn from(value: CliAccessMode) -> Self {
        match value {
            CliAccessMode::LiveReadOnly => Self::LiveReadOnly,
            CliAccessMode::ImmutableReadOnly => Self::ImmutableReadOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliQuerySyntax {
    NaturalTerms,
    ExactPhrase,
    AllTerms,
    AnyTerms,
    BackendNative,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliScriptureQuerySyntax {
    /// Literal prefix matching over normalized_reference.
    Prefix,
    /// Literal equality matching over normalized_reference.
    Exact,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum CliRetrievalPurpose {
    LocalUi,
    ModelContext,
    ExcerptExport,
}

impl From<CliRetrievalPurpose> for RetrievalPurpose {
    fn from(value: CliRetrievalPurpose) -> Self {
        match value {
            CliRetrievalPurpose::LocalUi => Self::LocalUi,
            CliRetrievalPurpose::ModelContext => Self::ModelContext,
            CliRetrievalPurpose::ExcerptExport => Self::ExcerptExport,
        }
    }
}

impl From<CliQuerySyntax> for QuerySyntax {
    fn from(value: CliQuerySyntax) -> Self {
        match value {
            CliQuerySyntax::NaturalTerms => Self::NaturalTerms,
            CliQuerySyntax::ExactPhrase => Self::ExactPhrase,
            CliQuerySyntax::AllTerms => Self::AllTerms,
            CliQuerySyntax::AnyTerms => Self::AnyTerms,
            CliQuerySyntax::BackendNative => Self::BackendNative,
        }
    }
}

impl From<CliScriptureQuerySyntax> for QuerySyntax {
    fn from(value: CliScriptureQuerySyntax) -> Self {
        match value {
            CliScriptureQuerySyntax::Prefix => Self::NaturalTerms,
            CliScriptureQuerySyntax::Exact => Self::ExactPhrase,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum StacTraversal {
    RootOnly,
    LatestChain,
    Children,
}

impl StacTraversal {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RootOnly => "root_only",
            Self::LatestChain => "latest_chain",
            Self::Children => "children",
        }
    }
}

struct CommandOutput {
    value: Value,
    exit_code: u8,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            let _print_result = error.print();
            return ExitCode::from(exit_code);
        }
    };

    match execute(cli) {
        Ok(output) => match write_pretty(io::stdout().lock(), &output.value) {
            Ok(()) => ExitCode::from(output.exit_code),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
            Err(error) => {
                let _write_result = write_error_report(&error);
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            let _write_result = write_error_report(error.as_ref());
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> CliResult<CommandOutput> {
    let (value, exit_code) = match cli.command {
        Command::Doctor(args) => doctor(&args)?,
        Command::Status(args) => (status(&args)?, 0),
        Command::Catalog { command } => (
            match command {
                CatalogCommand::Validate(args) => validate_catalog(&args)?,
                CatalogCommand::Search(args) => search_catalog(&args)?,
                CatalogCommand::Plan(args) => plan_catalog(&args)?,
            },
            0,
        ),
        Command::Sources { command } => (
            match command {
                SourcesCommand::Validate(args) => validate_sources(&args)?,
                SourcesCommand::List(args) => list_sources(&args)?,
            },
            0,
        ),
        Command::Discover { command } => (
            match command {
                DiscoveryCommand::KiwixOpds(args) => providers::discover_kiwix(&args)?,
                DiscoveryCommand::OvertureStac(args) => providers::discover_stac(&args)?,
                DiscoveryCommand::Metalink(args) => providers::discover_metalink(&args)?,
            },
            0,
        ),
        Command::Install(args) => (install(&args)?, 0),
        Command::RegisterSqlite(args) => (alexandria::register_sqlite(&args)?, 0),
        Command::Installed(args) => (installed(&args)?, 0),
        Command::Verify(args) => (verify(&args)?, 0),
        Command::ReceiptHistory(args) => (receipt_history(&args)?, 0),
        Command::Alexandria { command } => (
            match command {
                AlexandriaCommand::Search(args) => alexandria::search(&args)?,
                AlexandriaCommand::Block(args) => alexandria::block(&args)?,
            },
            0,
        ),
        Command::CommunityArchive { command } => (
            match command {
                CommunityCommand::Search(args) => community::search(&args)?,
                CommunityCommand::Message(args) => community::message(&args)?,
            },
            0,
        ),
        Command::Encyclopedia { command } => (
            match command {
                EncyclopediaCommand::Search(args) => encyclopedia::search(&args)?,
                EncyclopediaCommand::Article(args) => encyclopedia::article(&args)?,
            },
            0,
        ),
        Command::Scripture { command } => (
            match command {
                ScriptureCommand::Search(args) => scripture::search(&args)?,
                ScriptureCommand::Occurrence(args) => scripture::occurrence(&args)?,
                ScriptureCommand::Passage(args) => scripture::passage(&args)?,
            },
            0,
        ),
        Command::AgentTools => (json_value(&InformationHost::tool_definitions())?, 0),
    };
    Ok(CommandOutput { value, exit_code })
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    status: &'static str,
    message: String,
    details: Value,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    version: &'static str,
    checks: Vec<DoctorCheck>,
}

fn doctor(args: &DoctorArgs) -> CliResult<(Value, u8)> {
    let mut checks = Vec::new();
    match transport_client(&args.transport).and_then(|client| {
        let _policy = transport_policy(&args.transport)?;
        Ok(client)
    }) {
        Ok(_) => checks.push(DoctorCheck {
            name: "acquisition_client",
            status: "pass",
            message: "bounded HTTP/file acquisition client constructed; no request was made"
                .to_string(),
            details: json!({
                "connect_timeout_seconds": args.transport.connect_timeout_seconds,
                "request_timeout_seconds": args.transport.request_timeout_seconds,
                "max_redirects": args.transport.max_redirects,
                "allow_file_roots": args.transport.allow_file_roots,
                "allow_private_network": args.transport.allow_private_network,
            }),
        }),
        Err(error) => checks.push(DoctorCheck {
            name: "acquisition_client",
            status: "fail",
            message: error.to_string(),
            details: Value::Null,
        }),
    }

    if let Some(path) = &args.catalog {
        match load_catalog(path, args.max_catalog_bytes) {
            Ok(index) => checks.push(DoctorCheck {
                name: "catalog",
                status: "pass",
                message: format!("catalog {} is valid", path.display()),
                details: catalog_summary(&index)?,
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "catalog",
                status: "fail",
                message: error.to_string(),
                details: json!({"path": path}),
            }),
        }
    }

    if let Some(root) = &args.root {
        match ManagedStore::open(root).and_then(|store| {
            let snapshot = store.list()?;
            let partial = store.list_partial_installs()?;
            Ok((store, snapshot, partial))
        }) {
            Ok((store, snapshot, partial)) => checks.push(DoctorCheck {
                name: "managed_store",
                status: "pass",
                message: format!("managed store {} is readable", store.root().display()),
                details: json!({
                    "managed_installations": snapshot.managed.len(),
                    "external_registrations": snapshot.external.len(),
                    "partial_installations": partial.len(),
                }),
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "managed_store",
                status: "fail",
                message: error.to_string(),
                details: json!({"requested_root": root}),
            }),
        }
    }

    if let Some(path) = &args.alexandria {
        let config = AlexandriaBackendConfig::new(
            "doctor-alexandria",
            "Alexandria doctor probe",
            ResourceId::parse("local.doctor.alexandria")?,
            ReleaseId::parse("observed")?,
            RepresentationId::parse("sqlite")?,
            path,
            args.access_mode.into(),
            "operator-provided Alexandria source",
        );
        match AlexandriaBackend::open(config) {
            Ok(backend) => {
                let health = backend.health();
                let passed = matches!(
                    health.status,
                    information_native_retrieval::BackendHealthStatus::Ready
                );
                checks.push(DoctorCheck {
                    name: "alexandria",
                    status: if passed { "pass" } else { "fail" },
                    message: health.message,
                    details: json!({
                        "path": path,
                        "access_mode": ExternalAccessMode::from(args.access_mode),
                    }),
                });
            }
            Err(error) => checks.push(DoctorCheck {
                name: "alexandria",
                status: "fail",
                message: format!("{}: {}", error.code, error.safe_message),
                details: json!({"path": path}),
            }),
        }
    }

    let ok = checks.iter().all(|check| check.status == "pass");
    let value = json_value(&DoctorReport {
        ok,
        version: HOST_VERSION,
        checks,
    })?;
    Ok((value, if ok { 0 } else { 2 }))
}

fn status(args: &StatusArgs) -> CliResult<Value> {
    let index = load_catalog(&args.catalog, args.max_catalog_bytes)?;
    let host = InformationHost::with_components(
        index,
        ManagedStore::open(&args.root)?,
        AcquireClient::with_defaults()?,
        RetrievalRouter::new(),
    )?;
    json_value(&host.status()?)
}

fn validate_catalog(args: &CatalogFileArgs) -> CliResult<Value> {
    let index = load_catalog(&args.catalog, args.max_catalog_bytes)?;
    let mut summary = catalog_summary(&index)?;
    if let Value::Object(fields) = &mut summary {
        fields.insert("valid".to_string(), Value::Bool(true));
        fields.insert("path".to_string(), json!(&args.catalog));
    }
    Ok(summary)
}

fn validate_sources(args: &SourceRegistryFileArgs) -> CliResult<Value> {
    let registry = load_source_registry(&args.registry, args.max_bytes)?;
    let mut summary = source_registry_summary(&registry, &args.registry);
    if let Value::Object(fields) = &mut summary {
        fields.insert("valid".to_string(), Value::Bool(true));
    }
    Ok(summary)
}

fn list_sources(args: &SourceRegistryFileArgs) -> CliResult<Value> {
    let registry = load_source_registry(&args.registry, args.max_bytes)?;
    let mut summary = source_registry_summary(&registry, &args.registry);
    if let Value::Object(fields) = &mut summary {
        fields.insert("sources".to_string(), json!(&registry.sources));
    }
    Ok(summary)
}

fn source_registry_summary(registry: &SourceRegistry, path: &Path) -> Value {
    let support_count = |support| {
        registry
            .sources
            .iter()
            .filter(|source| source.support == support)
            .count()
    };
    json!({
        "schema": registry.schema,
        "path": path,
        "source_count": registry.sources.len(),
        "support_counts": {
            "shipped_discovery_and_exact_metalink":
                support_count(SourceRegistrySupport::ShippedDiscoveryAndExactMetalink),
            "shipped_bounded_discovery":
                support_count(SourceRegistrySupport::ShippedBoundedDiscovery),
            "registry_only":
                support_count(SourceRegistrySupport::RegistryOnly),
        },
        "network_access_performed": false,
        "operator_notice": "registry_only entries are descriptive upstream leads with no shipped provider adapter; registry membership never grants acquisition or installation authority",
    })
}

#[derive(Debug, Serialize)]
struct CatalogSearchItem<'catalog> {
    relevance: u32,
    record: &'catalog ResourceRecord,
    matching_representations: Vec<information_native_catalog::CatalogRepresentationMatch>,
}

fn search_catalog(args: &CatalogSearchArgs) -> CliResult<Value> {
    input::validate_values("languages", &args.language, 1_024, 512)?;
    input::validate_values("subjects", &args.subject, 1_024, 512)?;
    let index = load_catalog(&args.catalog, args.max_catalog_bytes)?;
    let query = CatalogSearchQuery {
        text: args.text.clone(),
        text_mode: args.mode.into(),
        kinds: args.kind.iter().copied().map(Into::into).collect(),
        languages: args.language.clone(),
        subjects: args.subject.clone(),
        formats: args.format.iter().copied().map(Into::into).collect(),
        capabilities: args.capability.iter().copied().map(Into::into).collect(),
        limit: args.limit,
    };
    let hits = index
        .search(&query)?
        .into_iter()
        .map(|hit| CatalogSearchItem {
            relevance: hit.relevance,
            record: hit.record,
            matching_representations: hit.matching_representations,
        })
        .collect::<Vec<_>>();
    json_value(&json!({
        "catalogue_id": index.catalog().catalogue_id,
        "catalogue_authority": index.authority(),
        "query": {
            "text": args.text,
            "mode": match args.mode {
                CatalogTextMode::AllTerms => "all_terms",
                CatalogTextMode::AnyTerm => "any_term",
            },
            "limit": args.limit,
        },
        "hits_returned": hits.len(),
        "hits": hits,
    }))
}

fn plan_catalog(args: &CatalogPlanArgs) -> CliResult<Value> {
    let index = load_catalog(&args.catalog, args.max_catalog_bytes)?;
    let mut mirrors = BTreeMap::new();
    for mirror in &args.mirror {
        let artifact_id = ArtifactId::parse(mirror.key.clone())?;
        if mirrors.insert(artifact_id, mirror.value.clone()).is_some() {
            return fail(format!(
                "mirror choice for artifact {:?} was supplied more than once",
                mirror.key
            ));
        }
    }
    let installation_id = match &args.installation_id {
        Some(value) => InstallationId::parse(value.clone())?,
        None => InstallationId::new(),
    };
    let request = PlanRequest {
        installation_id,
        resource_id: ResourceId::parse(args.resource_id.clone())?,
        release_id: ReleaseId::parse(args.release_id.clone())?,
        representation_id: RepresentationId::parse(args.representation_id.clone())?,
        selection: InstallSelection::default(),
        mirror_choices: mirrors,
        available_bytes_observed: args.available_bytes,
        created_at: Utc::now(),
    };
    json_value(&index.resolve_install_plan(request)?)
}

fn install(args: &InstallArgs) -> CliResult<Value> {
    let plan: InstallPlan =
        input::read_json_bounded(&args.plan, args.max_plan_bytes, "install plan")?;
    plan.validate()?;
    let host = InformationHost::with_components_and_policy(
        CatalogIndex::new(empty_catalog())?,
        ManagedStore::open(&args.root)?,
        transport_client(&args.transport)?,
        transport_policy(&args.transport)?,
        RetrievalRouter::new(),
    )?;
    json_value(&host.install_detached_plan(&plan)?)
}

fn installed(args: &InstalledArgs) -> CliResult<Value> {
    let store = ManagedStore::open(&args.root)?;
    let snapshot = store.list()?;
    let partial = store
        .list_partial_installs()?
        .into_iter()
        .map(|entry| {
            json!({
                "installation_id": entry.installation_id,
                "plan_sha256": entry.plan_sha256,
                "state": match entry.state {
                    PartialInstallState::Staged => "staged",
                    PartialInstallState::ActivatedUnregistered => "activated_unregistered",
                },
                "absolute_path": entry.absolute_path,
            })
        })
        .collect::<Vec<_>>();
    let receipts = store.list_receipts()?;
    Ok(json!({
        "root": store.root(),
        "managed_count": snapshot.managed.len(),
        "external_count": snapshot.external.len(),
        "partial_count": partial.len(),
        "managed": snapshot.managed,
        "external": snapshot.external,
        "partial": partial,
        "latest_managed_receipts": receipts,
    }))
}

fn verify(args: &VerifyArgs) -> CliResult<Value> {
    let store = ManagedStore::open(&args.root)?;
    let installation_id = InstallationId::parse(args.installation_id.clone())?;
    json_value(&store.verify_full(&installation_id)?)
}

fn receipt_history(args: &ReceiptHistoryArgs) -> CliResult<Value> {
    let store = ManagedStore::open(&args.root)?;
    let installation_id = InstallationId::parse(args.installation_id.clone())?;
    let history = store.receipt_history(&installation_id)?;
    Ok(json!({
        "installation_id": installation_id,
        "events": history,
    }))
}

fn load_catalog(path: &Path, max_bytes: u64) -> CliResult<CatalogIndex> {
    let bytes = input::read_bounded(path, max_bytes)?;
    let parser_limit = usize::try_from(max_bytes)
        .map_err(|_| message_error("catalog byte limit does not fit this platform"))?;
    Ok(CatalogIndex::from_json_slice_with_limit(
        &bytes,
        parser_limit,
    )?)
}

fn load_source_registry(path: &Path, max_bytes: u64) -> CliResult<SourceRegistry> {
    let bytes = input::read_bounded(path, max_bytes)?;
    let parser_limit = usize::try_from(max_bytes)
        .map_err(|_| message_error("source registry byte limit does not fit this platform"))?;
    Ok(parse_source_registry_with_limit(&bytes, parser_limit)?)
}

fn catalog_summary(index: &CatalogIndex) -> CliResult<Value> {
    let catalog = index.catalog();
    let release_count = checked_sum(
        catalog.resources.iter().map(|record| record.releases.len()),
        "release count",
    )?;
    let representation_count = checked_sum(
        catalog
            .resources
            .iter()
            .flat_map(|record| &record.releases)
            .map(|release| release.representations.len()),
        "representation count",
    )?;
    let artifact_count = checked_sum(
        catalog
            .resources
            .iter()
            .flat_map(|record| &record.releases)
            .flat_map(|release| &release.representations)
            .map(|representation| representation.artifacts.len()),
        "artifact count",
    )?;
    Ok(json!({
        "schema": catalog.schema,
        "catalogue_id": catalog.catalogue_id,
        "generated_at": catalog.generated_at,
        "publisher": catalog.publisher,
        "declared_trust": catalog.declared_trust,
        "catalogue_authority": index.authority(),
        "resources": catalog.resources.len(),
        "releases": release_count,
        "representations": representation_count,
        "artifacts": artifact_count,
    }))
}

fn checked_sum(values: impl IntoIterator<Item = usize>, label: &str) -> CliResult<usize> {
    values.into_iter().try_fold(0_usize, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| message_error(format!("{label} overflow")))
    })
}

fn empty_catalog() -> InformationCatalog {
    InformationCatalog {
        schema: information_native_types::CATALOG_SCHEMA.to_string(),
        catalogue_id: "information-native-cli-install".to_string(),
        generated_at: Utc::now(),
        publisher: Publisher {
            name: "information-native CLI".to_string(),
            homepage: None,
        },
        declared_trust: CatalogTrust::BuiltIn,
        resources: Vec::new(),
    }
}

pub(crate) fn transport_client(args: &TransportArgs) -> CliResult<AcquireClient> {
    Ok(AcquireClient::new(AcquireConfig {
        connect_timeout: Duration::from_secs(args.connect_timeout_seconds),
        request_timeout: Duration::from_secs(args.request_timeout_seconds),
        max_redirects: args.max_redirects,
        user_agent: format!("information-native-cli/{HOST_VERSION}"),
    })?)
}

pub(crate) fn transport_policy(args: &TransportArgs) -> CliResult<AcquisitionPolicy> {
    let mut policy = AcquisitionPolicy::restricted();
    if args.allow_private_network {
        policy = policy.with_network_scope(NetworkScope::AnyAddress);
    }
    for root in &args.allow_file_roots {
        policy.grant_file_root(root)?;
    }
    Ok(policy)
}

fn parse_key_value(value: &str) -> Result<KeyValueArg, String> {
    let Some((key, value)) = value.split_once('=') else {
        return Err("value must use KEY=VALUE form".to_string());
    };
    if key.trim().is_empty() || key.len() > 192 {
        return Err("KEY must contain between 1 and 192 bytes".to_string());
    }
    if value.trim().is_empty() || value.len() > 8_192 {
        return Err("VALUE must contain between 1 and 8192 bytes".to_string());
    }
    Ok(KeyValueArg {
        key: key.to_string(),
        value: value.to_string(),
    })
}

fn parse_search_text(value: &str) -> Result<String, String> {
    parse_bounded_text(value, 8_192, "search text")
}

fn parse_lookup_key(value: &str) -> Result<String, String> {
    parse_bounded_text(value, 8_192, "lookup key")
}

fn parse_scripture_reference(value: &str) -> Result<String, String> {
    parse_bounded_text(value, 512, "Scripture reference")
}

fn parse_positive_record_id(value: &str) -> Result<String, String> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| "record id must be a positive signed 64-bit integer".to_string())?;
    if parsed <= 0 {
        return Err("record id must be a positive signed 64-bit integer".to_string());
    }
    Ok(value.to_string())
}

fn parse_nonempty_text(value: &str) -> Result<String, String> {
    parse_bounded_text(value, 512, "text")
}

fn parse_bounded_text(value: &str, max_chars: usize, label: &str) -> Result<String, String> {
    let chars = value.chars().count();
    if value.trim().is_empty() || chars > max_chars {
        return Err(format!(
            "{label} must contain between 1 and {max_chars} characters"
        ));
    }
    Ok(value.to_string())
}

fn parse_sha256(value: &str) -> Result<String, String> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SHA-256 must contain exactly 64 hexadecimal digits".to_string());
    }
    Ok(digest.to_ascii_lowercase())
}

fn parse_catalog_limit(value: &str) -> Result<usize, String> {
    parse_bounded_usize(value, 1, 10_000, "catalog search limit")
}

fn parse_provider_pages(value: &str) -> Result<usize, String> {
    parse_bounded_usize(value, 1, MAX_PROVIDER_PAGES, "provider page limit")
}

fn parse_provider_documents(value: &str) -> Result<usize, String> {
    parse_bounded_usize(value, 1, MAX_PROVIDER_DOCUMENTS, "provider document limit")
}

fn parse_provider_records(value: &str) -> Result<usize, String> {
    parse_bounded_usize(value, 1, MAX_PROVIDER_RECORDS, "provider record limit")
}

fn parse_redirect_limit(value: &str) -> Result<usize, String> {
    parse_bounded_usize(value, 0, 16, "redirect limit")
}

fn parse_bounded_usize(
    value: &str,
    minimum: usize,
    maximum: usize,
    label: &str,
) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{label} is not a non-negative integer"))?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(format!("{label} must be between {minimum} and {maximum}"));
    }
    Ok(parsed)
}

pub(crate) fn json_value<T: Serialize>(value: &T) -> CliResult<Value> {
    Ok(serde_json::to_value(value)?)
}

#[derive(Debug)]
struct MessageError(String);

impl fmt::Display for MessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for MessageError {}

pub(crate) fn message_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(MessageError(message.into()))
}

pub(crate) fn fail<T>(message: impl Into<String>) -> CliResult<T> {
    Err(message_error(message))
}

fn write_pretty(mut writer: impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

fn write_error_report(error: &dyn Error) -> io::Result<()> {
    let mut causes = Vec::new();
    let mut source = error.source();
    for _ in 0..16 {
        let Some(cause) = source else {
            break;
        };
        causes.push(cause.to_string());
        source = cause.source();
    }
    write_pretty(
        io::stderr().lock(),
        &json!({
            "ok": false,
            "error": {
                "message": error.to_string(),
                "causes": causes,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_integer_parser_accepts_only_its_closed_interval() -> CliResult<()> {
        assert_eq!(parse_bounded_usize("4", 1, 4, "test")?, 4);
        assert!(parse_bounded_usize("0", 1, 4, "test").is_err());
        assert!(parse_bounded_usize("5", 1, 4, "test").is_err());
        Ok(())
    }

    #[test]
    fn key_value_parser_splits_only_once() -> CliResult<()> {
        let value = parse_key_value("theme_tag=love=agape")?;
        assert_eq!(value.key, "theme_tag");
        assert_eq!(value.value, "love=agape");
        Ok(())
    }

    #[test]
    fn clap_rejects_unbounded_discovery_page_count() {
        let result = Cli::try_parse_from([
            "information-native",
            "discover",
            "kiwix-opds",
            "--max-pages",
            "101",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn sources_commands_require_and_preserve_an_explicit_registry_path() -> CliResult<()> {
        assert!(Cli::try_parse_from(["information-native", "sources", "validate"]).is_err());
        let cli = Cli::try_parse_from([
            "information-native",
            "sources",
            "list",
            "--registry",
            "/tmp/sources.json",
        ])?;
        let Command::Sources {
            command: SourcesCommand::List(args),
        } = cli.command
        else {
            return fail("parsed the wrong sources command");
        };
        assert_eq!(args.registry, Path::new("/tmp/sources.json"));
        Ok(())
    }

    #[test]
    fn shipped_source_registry_lists_registry_only_as_metadata_not_execution() -> CliResult<()> {
        let registry =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalogues/default-sources.json");
        let output = execute(Cli::try_parse_from([
            "information-native",
            "sources",
            "list",
            "--registry",
            registry
                .to_str()
                .ok_or_else(|| message_error("test registry path is not UTF-8"))?,
        ])?)?;

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.value["network_access_performed"], false);
        assert!(
            output.value["source_count"]
                .as_u64()
                .is_some_and(|count| count > 2)
        );
        assert!(
            output.value["support_counts"]["registry_only"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert!(
            output.value["operator_notice"]
                .as_str()
                .is_some_and(|notice| notice.contains("no shipped provider adapter"))
        );
        assert!(output.value["sources"].as_array().is_some_and(|sources| {
            sources.iter().any(|source| {
                source["id"] == "geofabrik-osm-extracts" && source["support"] == "registry_only"
            })
        }));
        Ok(())
    }

    #[test]
    fn empty_install_catalog_is_contract_valid() -> CliResult<()> {
        empty_catalog().validate()?;
        Ok(())
    }

    #[test]
    fn community_model_context_opt_in_is_explicitly_parsed() -> CliResult<()> {
        let cli = Cli::try_parse_from([
            "information-native",
            "community-archive",
            "search",
            "--database",
            "/tmp/community.sqlite",
            "--resource-id",
            "local.community",
            "--release-id",
            "observed",
            "--representation-id",
            "sqlite",
            "--publisher",
            "private archive custodian",
            "--text",
            "attention",
            "--purpose",
            "model-context",
            "--use-policy-json",
            "/tmp/community-policy.json",
            "--allow-private-model-context",
        ])?;
        let Command::CommunityArchive {
            command: CommunityCommand::Search(args),
        } = cli.command
        else {
            return fail("parsed the wrong Community Archive command");
        };
        assert!(args.source.allow_private_model_context);
        assert!(matches!(args.purpose, CliRetrievalPurpose::ModelContext));
        assert_eq!(
            args.source.use_policy_json.as_deref(),
            Some(Path::new("/tmp/community-policy.json"))
        );
        Ok(())
    }

    #[test]
    fn encyclopedia_policy_files_and_article_id_are_bounded() -> CliResult<()> {
        let valid = Cli::try_parse_from([
            "information-native",
            "encyclopedia",
            "article",
            "--database",
            "/tmp/encyclopedia.sqlite",
            "--resource-id",
            "local.encyclopedia",
            "--release-id",
            "observed",
            "--representation-id",
            "articles",
            "--publisher",
            "archive custodian",
            "--rights-json",
            "/tmp/rights.json",
            "--use-policy-json",
            "/tmp/policy.json",
            "--article-id",
            "42",
        ])?;
        let Command::Encyclopedia {
            command: EncyclopediaCommand::Article(args),
        } = valid.command
        else {
            return fail("parsed the wrong encyclopedia command");
        };
        assert_eq!(args.article_id, "42");
        assert_eq!(
            args.source.rights_json.as_deref(),
            Some(Path::new("/tmp/rights.json"))
        );

        let invalid = Cli::try_parse_from([
            "information-native",
            "encyclopedia",
            "article",
            "--database",
            "/tmp/encyclopedia.sqlite",
            "--resource-id",
            "local.encyclopedia",
            "--release-id",
            "observed",
            "--representation-id",
            "articles",
            "--publisher",
            "archive custodian",
            "--article-id",
            "0",
        ]);
        assert!(invalid.is_err());
        Ok(())
    }

    #[test]
    fn scripture_exposes_only_supported_search_modes() -> CliResult<()> {
        let cli = Cli::try_parse_from([
            "information-native",
            "scripture",
            "search",
            "--database",
            "/tmp/scripture.sqlite",
            "--resource-id",
            "local.scripture",
            "--release-id",
            "observed",
            "--representation-id",
            "references",
            "--publisher",
            "index custodian",
            "--reference",
            "John 3:16",
            "--syntax",
            "exact",
            "--document-id",
            "alexandria-document-1",
        ])?;
        let Command::Scripture {
            command: ScriptureCommand::Search(args),
        } = cli.command
        else {
            return fail("parsed the wrong Scripture command");
        };
        assert!(matches!(args.syntax, CliScriptureQuerySyntax::Exact));
        assert_eq!(args.document_ids, ["alexandria-document-1"]);
        Ok(())
    }
}
