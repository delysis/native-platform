use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const CLASSIFICATION_PATH: &str = "ci/lean/file-classification.json";
const REACHABILITY_PATH: &str = "ci/lean/reachability.json";
const BASELINE_PATH: &str = "ci/lean/w8-baseline.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Classification {
    schema_version: u64,
    path_universe: String,
    classes: Vec<String>,
    match_semantics: String,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    class: String,
    #[serde(default)]
    exact: Vec<String>,
    #[serde(default)]
    prefixes: Vec<String>,
    #[serde(default)]
    suffixes: Vec<String>,
}

#[derive(Debug, Clone)]
struct GitEntry {
    path: String,
    object: String,
    bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClassMeasure {
    files: u64,
    bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceMeasure {
    rust_files: u64,
    rust_nonblank_lines: u64,
    production_rust_nonblank_lines: u64,
    test_rust_nonblank_lines: u64,
    tooling_rust_nonblank_lines: u64,
    generated_nonblank_lines: u64,
    frontend_files: u64,
    frontend_nonblank_lines: u64,
    public_rust_items_lexical: u64,
    fixture_data_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ShippingGraph {
    roots: BTreeMap<String, Vec<String>>,
    first_party_packages: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Baseline {
    schema_version: u64,
    milestone: String,
    source_revision: String,
    source_tree: String,
    metric_contract: MetricContract,
    tracked_files: u64,
    classes: BTreeMap<String, ClassMeasure>,
    source: SourceMeasure,
    workspace_packages: u64,
    first_party_git_dependencies: u64,
    acceptance_thresholds: AcceptanceThresholds,
    shipping_graph: ShippingGraph,
    known_binary_bytes: BTreeMap<String, u64>,
    timings: TimingMeasure,
    classification_sha256: String,
    reachability_sha256: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TimingMeasure {
    local_workspace_build_ms: Option<u64>,
    local_build_cache_state: String,
    required_ci_wall_ms: Option<u64>,
    note: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptanceThresholds {
    maximum_rust_nonblank_lines: u64,
    maximum_workspace_packages: u64,
    maximum_public_rust_items_lexical: u64,
    maximum_first_party_git_dependencies: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MetricContract {
    nonblank_line: String,
    production_rust_classes: Vec<String>,
    test_rust_class: String,
    tooling_rust_classes: Vec<String>,
    public_item_method: String,
    frontend_suffixes: Vec<String>,
    package_method: String,
    graph_method: String,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    workspace_members: Vec<String>,
    resolve: MetadataResolve,
}

#[derive(Debug, Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MetadataResolve {
    nodes: Vec<MetadataNode>,
}

#[derive(Debug, Deserialize)]
struct MetadataNode {
    id: String,
    dependencies: Vec<String>,
}

pub(super) fn run(root: &Path, arguments: Vec<String>) -> Result<()> {
    match arguments.as_slice() {
        [command] if command == "verify" => verify(root),
        [command] if command == "snapshot" => snapshot(root, "HEAD"),
        [command, revision] if command == "snapshot" => snapshot(root, revision),
        _ => bail!("usage: cargo xtask lean <verify|snapshot [revision]>"),
    }
}

fn verify(root: &Path) -> Result<()> {
    let classification = read_classification(root)?;
    validate_reachability(root)?;
    let entries = git_entries(root, "HEAD")?;
    validate_classification(&classification, &entries)?;

    let baseline: Baseline = serde_json::from_str(
        &fs::read_to_string(root.join(BASELINE_PATH)).context("read W8 lean baseline")?,
    )
    .context("parse W8 lean baseline")?;
    ensure!(baseline.schema_version == 1);
    ensure!(baseline.milestone == "W8-HOME");
    ensure!(baseline.source_revision.len() == 40);
    ensure!(
        baseline.source_tree
            == git(
                root,
                [
                    "rev-parse",
                    &format!("{}^{{tree}}", baseline.source_revision)
                ]
            )?
            .trim()
    );
    ensure!(
        baseline.classification_sha256 == digest_file(&root.join(CLASSIFICATION_PATH))?,
        "lean classification changed; regenerate and review the baseline"
    );
    ensure!(
        baseline.reachability_sha256 == digest_file(&root.join(REACHABILITY_PATH))?,
        "lean reachability changed; regenerate and review the baseline"
    );
    let measured = measure(
        root,
        &baseline.source_revision,
        &classification,
        &git_entries(root, &baseline.source_revision)?,
    )?;
    ensure!(
        baseline_without_timings(&baseline) == baseline_without_timings(&measured),
        "W8 lean baseline drift; regenerate and review the baseline"
    );
    println!(
        "native-platform lean policy: pass ({} tracked paths classified; W8 {} / {})",
        entries.len(),
        baseline.source_revision,
        baseline.source_tree
    );
    Ok(())
}

fn baseline_without_timings(baseline: &Baseline) -> Value {
    let mut value = serde_json::to_value(baseline).expect("baseline serializes");
    value
        .as_object_mut()
        .expect("baseline is an object")
        .remove("timings");
    value
}

fn snapshot(root: &Path, revision: &str) -> Result<()> {
    let revision = git(root, ["rev-parse", revision])?.trim().to_owned();
    ensure!(
        revision.len() == 40,
        "snapshot revision must resolve to a commit"
    );
    ensure_cargo_graph_matches_revision(root, &revision)?;
    let classification = read_classification(root)?;
    validate_reachability(root)?;
    let entries = git_entries(root, &revision)?;
    validate_classification(&classification, &entries)?;
    let baseline = measure(root, &revision, &classification, &entries)?;
    println!("{}", serde_json::to_string_pretty(&baseline)?);
    Ok(())
}

fn read_classification(root: &Path) -> Result<Classification> {
    let classification: Classification = serde_json::from_str(
        &fs::read_to_string(root.join(CLASSIFICATION_PATH)).context("read lean classification")?,
    )
    .context("parse lean classification")?;
    let expected = [
        "shipping production",
        "optional product feature",
        "research",
        "diagnostic",
        "test",
        "fixture",
        "generated",
        "migration-only",
        "legacy compatibility",
        "documentation",
        "tooling",
        "unused",
    ];
    ensure!(classification.schema_version == 1);
    ensure!(classification.classes == expected);
    ensure!(classification.path_universe == "git ls-files --cached");
    ensure!(
        classification
            .match_semantics
            .starts_with("Rules are evaluated in array order;")
    );
    for (index, rule) in classification.rules.iter().enumerate() {
        ensure!(
            expected.contains(&rule.class.as_str()),
            "unknown class in rule {index}"
        );
        ensure!(
            !(rule.exact.is_empty() && rule.prefixes.is_empty() && rule.suffixes.is_empty()),
            "selector-free classification rule {index}"
        );
        for prefix in &rule.prefixes {
            ensure!(!prefix.is_empty(), "empty prefix in rule {index}");
        }
        for suffix in &rule.suffixes {
            ensure!(!suffix.is_empty(), "empty suffix in rule {index}");
        }
        for exact in &rule.exact {
            ensure!(!exact.is_empty(), "empty exact path in rule {index}");
        }
    }
    Ok(classification)
}

fn validate_reachability(root: &Path) -> Result<()> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(root.join(REACHABILITY_PATH)).context("read lean reachability")?,
    )
    .context("parse lean reachability")?;
    ensure!(value["schema_version"] == 1);
    let journeys = value["journeys"]
        .as_array()
        .context("reachability journeys")?;
    let expected = BTreeSet::from([
        "fte-gateway-loopback",
        "mom-local-chat-persona-attachment-quit",
        "loom-quiet-writing-suggestion-promotion-quit",
        "information-install-query-core",
        "speech-core-selected-backends",
    ]);
    let actual = journeys
        .iter()
        .map(|journey| journey["id"].as_str().context("journey id"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(actual == expected, "accepted journey inventory drift");
    for journey in journeys {
        ensure!(
            !journey["entry_packages"]
                .as_array()
                .context("journey entry_packages")?
                .is_empty()
        );
        for path in journey["module_roots"]
            .as_array()
            .context("journey module_roots")?
        {
            let path = path.as_str().context("module root path")?;
            ensure!(
                root.join(path).exists(),
                "missing journey module root: {path}"
            );
        }
    }
    Ok(())
}

fn validate_classification(classification: &Classification, entries: &[GitEntry]) -> Result<()> {
    let mut unmatched = Vec::new();
    for entry in entries {
        if classify(classification, &entry.path).is_none() {
            unmatched.push(entry.path.as_str());
        }
    }
    ensure!(
        unmatched.is_empty(),
        "unclassified tracked paths: {}",
        unmatched.join(", ")
    );
    Ok(())
}

fn classify<'a>(classification: &'a Classification, path: &str) -> Option<&'a str> {
    classification.rules.iter().find_map(|rule| {
        let matched = rule.exact.iter().any(|value| value == path)
            || rule.prefixes.iter().any(|value| path.starts_with(value))
            || rule.suffixes.iter().any(|value| path.ends_with(value));
        matched.then_some(rule.class.as_str())
    })
}

fn measure(
    root: &Path,
    revision: &str,
    classification: &Classification,
    entries: &[GitEntry],
) -> Result<Baseline> {
    let blobs = read_blobs(root, entries)?;
    let mut classes = classification
        .classes
        .iter()
        .map(|class| (class.clone(), ClassMeasure::default()))
        .collect::<BTreeMap<_, _>>();
    let mut source = SourceMeasure::default();
    let frontend_suffixes = [
        ".css", ".html", ".js", ".jsx", ".mjs", ".svelte", ".ts", ".tsx",
    ];
    for (entry, bytes) in entries.iter().zip(blobs) {
        let class = classify(classification, &entry.path).context("classified entry")?;
        let measure = classes.get_mut(class).context("known class")?;
        measure.files += 1;
        measure.bytes += entry.bytes;
        if class == "fixture" {
            source.fixture_data_bytes += entry.bytes;
        }
        let text = String::from_utf8_lossy(&bytes);
        let nonblank = text.lines().filter(|line| !line.trim().is_empty()).count() as u64;
        if class == "generated" {
            source.generated_nonblank_lines += nonblank;
        }
        if entry.path.ends_with(".rs") {
            source.rust_files += 1;
            source.rust_nonblank_lines += nonblank;
            match class {
                "shipping production"
                | "optional product feature"
                | "research"
                | "legacy compatibility" => {
                    source.production_rust_nonblank_lines += nonblank;
                    source.public_rust_items_lexical += lexical_public_items(&text);
                }
                "test" => source.test_rust_nonblank_lines += nonblank,
                "tooling" | "diagnostic" => source.tooling_rust_nonblank_lines += nonblank,
                _ => {}
            }
        } else if frontend_suffixes
            .iter()
            .any(|suffix| entry.path.ends_with(suffix))
        {
            source.frontend_files += 1;
            source.frontend_nonblank_lines += nonblank;
        }
    }
    let metadata = cargo_metadata(root)?;
    let workspace_ids = metadata
        .workspace_members
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let first_party_git_dependencies = metadata
        .packages
        .iter()
        .filter(|package| {
            package.source.as_deref().is_some_and(|source| {
                source.starts_with("git+") && source.contains("github.com/delysis/")
            }) && !source_is_external_ffi(package)
        })
        .count() as u64;
    let shipping_graph = shipping_graph(root, &metadata, &workspace_ids)?;
    let home: Value = serde_json::from_str(
        &fs::read_to_string(root.join("migration/home.json")).context("read HOME receipt")?,
    )?;
    let known_binary_bytes = BTreeMap::from([
        (
            "loom_macos_executable".to_owned(),
            home["local_acceptance"]["loom_gemma"]["bundle"]["executable_bytes"]
                .as_u64()
                .context("Loom executable bytes")?,
        ),
        (
            "speech_apple_test_executable".to_owned(),
            home["local_acceptance"]["speech"]["apple"]["executable_bytes"]
                .as_u64()
                .context("Apple speech executable bytes")?,
        ),
    ]);
    let acceptance_thresholds = AcceptanceThresholds {
        maximum_rust_nonblank_lines: source.rust_nonblank_lines * 4 / 5,
        maximum_workspace_packages: workspace_ids.len() as u64 * 3 / 4,
        maximum_public_rust_items_lexical: source.public_rust_items_lexical,
        maximum_first_party_git_dependencies: 0,
    };
    Ok(Baseline {
        schema_version: 1,
        milestone: "W8-HOME".to_owned(),
        source_revision: revision.to_owned(),
        source_tree: git(root, ["rev-parse", &format!("{revision}^{{tree}}")])?.trim().to_owned(),
        metric_contract: MetricContract {
            nonblank_line: "UTF-8-lossy logical lines whose trimmed form is nonempty; files are assigned wholly by path class".to_owned(),
            production_rust_classes: vec!["shipping production", "optional product feature", "research", "legacy compatibility"].into_iter().map(str::to_owned).collect(),
            test_rust_class: "test".to_owned(),
            tooling_rust_classes: vec!["tooling", "diagnostic"].into_iter().map(str::to_owned).collect(),
            public_item_method: "lexical line count of externally public Rust item declarations; pub(crate), fields, and re-exports are excluded".to_owned(),
            frontend_suffixes: frontend_suffixes.into_iter().map(str::to_owned).collect(),
            package_method: "cargo metadata --locked --format-version 1 workspace_members".to_owned(),
            graph_method: "transitive cargo metadata resolve closure, retaining only workspace package names for declared shipping roots".to_owned(),
        },
        tracked_files: entries.len() as u64,
        classes,
        source,
        workspace_packages: workspace_ids.len() as u64,
        first_party_git_dependencies,
        acceptance_thresholds,
        shipping_graph,
        known_binary_bytes,
        timings: TimingMeasure {
            local_workspace_build_ms: None,
            local_build_cache_state: "Unmeasured.".to_owned(),
            required_ci_wall_ms: None,
            note: "Populate from a timed W8 worktree build and the required W8 policy/macOS workflow timestamps; null is pending measurement, never zero.".to_owned(),
        },
        classification_sha256: digest_file(&root.join(CLASSIFICATION_PATH))?,
        reachability_sha256: digest_file(&root.join(REACHABILITY_PATH))?,
    })
}

fn lexical_public_items(text: &str) -> u64 {
    text.lines()
        .map(str::trim_start)
        .filter(|line| {
            [
                "pub async fn ",
                "pub const ",
                "pub enum ",
                "pub extern ",
                "pub fn ",
                "pub macro ",
                "pub mod ",
                "pub static ",
                "pub struct ",
                "pub trait ",
                "pub type ",
                "pub unsafe fn ",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .count() as u64
}

fn source_is_external_ffi(package: &MetadataPackage) -> bool {
    package.name.starts_with("llama-cpp-")
}

fn cargo_metadata(root: &Path) -> Result<Metadata> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(root)
        .output()
        .context("run cargo metadata for lean census")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parse cargo metadata for lean census")
}

fn shipping_graph(
    root: &Path,
    metadata: &Metadata,
    workspace_ids: &BTreeSet<String>,
) -> Result<ShippingGraph> {
    let reachability: Value =
        serde_json::from_str(&fs::read_to_string(root.join(REACHABILITY_PATH))?)?;
    let journeys = reachability["journeys"]
        .as_array()
        .context("reachability journeys")?;
    let names = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package.id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let package_names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let edges = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.dependencies.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let mut roots = BTreeMap::new();
    let mut first_party_packages = BTreeMap::new();
    for journey in journeys {
        let id = journey["id"].as_str().context("journey id")?;
        let profile_roots = journey["entry_packages"]
            .as_array()
            .context("journey entry packages")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .context("shipping root package")
                    .map(str::to_owned)
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(!profile_roots.is_empty(), "empty journey roots: {id}");
        let mut queue = VecDeque::new();
        for name in &profile_roots {
            queue.push_back(
                *names
                    .get(name.as_str())
                    .with_context(|| format!("unknown shipping root package {name}"))?,
            );
        }
        let mut seen = BTreeSet::new();
        while let Some(package) = queue.pop_front() {
            if seen.insert(package) {
                for dependency in edges
                    .get(package)
                    .into_iter()
                    .flat_map(|dependencies| dependencies.iter())
                {
                    queue.push_back(dependency);
                }
            }
        }
        let closure = seen
            .into_iter()
            .filter(|id| workspace_ids.contains(*id))
            .map(|id| {
                package_names
                    .get(id)
                    .context("package name for closure")
                    .map(|name| (*name).to_owned())
            })
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect();
        roots.insert(id.to_owned(), profile_roots);
        first_party_packages.insert(id.to_owned(), closure);
    }
    Ok(ShippingGraph {
        roots,
        first_party_packages,
    })
}

fn ensure_cargo_graph_matches_revision(root: &Path, revision: &str) -> Result<()> {
    let manifests = git(root, ["ls-tree", "-r", "--name-only", revision])?
        .lines()
        .filter(|path| *path == "Cargo.lock" || path.ends_with("Cargo.toml"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for path in manifests {
        let expected = git_bytes(root, &["show", &format!("{revision}:{path}")])?;
        let actual = fs::read(root.join(&path)).with_context(|| format!("read worktree {path}"))?;
        ensure!(
            actual == expected,
            "worktree {path} differs from snapshot revision; baseline graph would be false"
        );
    }
    Ok(())
}

fn git_entries(root: &Path, revision: &str) -> Result<Vec<GitEntry>> {
    let bytes = git_bytes(root, &["ls-tree", "-rz", "--long", revision])?;
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("ls-tree tab")?;
        let metadata = std::str::from_utf8(&record[..tab]).context("ls-tree metadata")?;
        let mut fields = metadata.split_ascii_whitespace();
        let _mode = fields.next().context("ls-tree mode")?;
        let kind = fields.next().context("ls-tree kind")?;
        let object = fields.next().context("ls-tree object")?.to_owned();
        let size = fields.next().context("ls-tree size")?;
        ensure!(
            kind == "blob",
            "non-blob tracked object is outside lean census"
        );
        let path = std::str::from_utf8(&record[tab + 1..])
            .context("UTF-8 tracked path")?
            .to_owned();
        entries.push(GitEntry {
            path,
            object,
            bytes: size.parse().context("ls-tree blob size")?,
        });
    }
    Ok(entries)
}

fn read_blobs(root: &Path, entries: &[GitEntry]) -> Result<Vec<Vec<u8>>> {
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawn git cat-file")?;
    {
        let stdin = child.stdin.as_mut().context("git cat-file stdin")?;
        for entry in entries {
            writeln!(stdin, "{}", entry.object)?;
        }
    }
    let mut stdout = BufReader::new(child.stdout.take().context("git cat-file stdout")?);
    let mut blobs = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut header = String::new();
        stdout.read_line(&mut header)?;
        let mut fields = header.split_ascii_whitespace();
        let object = fields.next().context("cat-file object")?;
        ensure!(object == entry.object);
        ensure!(fields.next() == Some("blob"));
        let size: usize = fields.next().context("cat-file size")?.parse()?;
        ensure!(size as u64 == entry.bytes);
        let mut bytes = vec![0; size];
        stdout.read_exact(&mut bytes)?;
        let mut newline = [0];
        stdout.read_exact(&mut newline)?;
        ensure!(newline == [b'\n']);
        blobs.push(bytes);
    }
    ensure!(child.wait()?.success(), "git cat-file failed");
    Ok(blobs)
}

fn digest_file(path: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(fs::read(path).with_context(|| format!("read {}", path.display()))?)
    ))
}

fn git<const N: usize>(root: &Path, arguments: [&str; N]) -> Result<String> {
    String::from_utf8(git_bytes(root, &arguments)?).context("Git returned non-UTF-8 text")
}

fn git_bytes(root: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .context("run git")?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
