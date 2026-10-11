use crate::{FriendsError, invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const DOTFILE: &str = ".community-archive.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FriendsConfig {
    pub version: u32,
    pub archive: PathBuf,
    pub friends: BTreeMap<String, FriendConfig>,
    pub retrieval: RetrievalSettings,
    pub prompt: PromptSettings,
    pub optimizer: OptimizerSettings,
    pub critique: CritiqueSettings,
    pub sampling: Option<crate::SamplingSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompting: Option<crate::PromptingSettings>,
}

impl Default for FriendsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            archive: PathBuf::from("data/community_archive.sqlite"),
            friends: BTreeMap::new(),
            retrieval: RetrievalSettings::default(),
            prompt: PromptSettings::default(),
            optimizer: OptimizerSettings::default(),
            critique: CritiqueSettings::default(),
            sampling: None,
            prompting: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FriendConfig {
    pub handle: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Prefer stable account IDs when a handle has changed or been reused.
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetrievalSettings {
    pub candidates_per_friend: usize,
    pub max_friends: usize,
    pub max_queries: usize,
    pub timeout_seconds: u64,
    pub queries: Vec<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub include_recent: bool,
    /// Explicit, inspectable query stopwords. Empty retains archive term parsing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_terms: Vec<String>,
}

impl Default for RetrievalSettings {
    fn default() -> Self {
        Self {
            candidates_per_friend: 96,
            max_friends: 6,
            max_queries: 6,
            timeout_seconds: 30,
            queries: Vec::new(),
            since: None,
            until: None,
            include_recent: true,
            ignored_terms: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PromptSettings {
    /// Unicode character budget, including instructions, draft and evidence.
    /// Native-kit performs the separate authoritative tokenizer check.
    pub max_chars: usize,
    pub max_excerpt_chars: usize,
    pub excerpts_per_friend: usize,
    pub direction: String,
}

impl Default for PromptSettings {
    fn default() -> Self {
        Self {
            max_chars: 32_000,
            max_excerpt_chars: 1_600,
            excerpts_per_friend: 16,
            direction: "Find the surprising connections, preserve productive disagreements, and turn them into concrete possibilities.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Recipe {
    Resonance,
    Constellation,
    Counterpoint,
}

impl Recipe {
    pub fn label(self) -> &'static str {
        match self {
            Self::Resonance => "Resonance",
            Self::Constellation => "Constellation",
            Self::Counterpoint => "Counterpoint",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OptimizerSettings {
    pub enabled: bool,
    pub recipes: Vec<Recipe>,
}

impl Default for OptimizerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            recipes: vec![
                Recipe::Resonance,
                Recipe::Constellation,
                Recipe::Counterpoint,
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CritiqueSettings {
    pub enabled: bool,
    pub max_rounds: usize,
    pub max_output_tokens: u32,
    pub timeout_seconds: u64,
}

impl Default for CritiqueSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rounds: 1,
            max_output_tokens: 2_048,
            timeout_seconds: 90,
        }
    }
}

fn bounded(name: &str, n: usize, min: usize, max: usize) -> Result<(), FriendsError> {
    if !(min..=max).contains(&n) {
        return Err(invalid(format!("{name} must be in {min}..={max}")));
    }
    Ok(())
}

pub(crate) fn valid_handle(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl FriendsConfig {
    /// Read exactly the selected file. No ancestor traversal or hidden merges.
    /// Relative archive paths are anchored to the dotfile, never the process cwd.
    pub fn load(path: &Path) -> Result<Self, FriendsError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        let before = file.metadata()?;
        if !before.is_file() || before.len() > 65_536 {
            return Err(invalid("dotfile must be a regular file of at most 64 KiB"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.nlink() != 1 {
                return Err(invalid("dotfile must have one filesystem link"));
            }
        }
        let mut text = String::new();
        (&file).take(65_537).read_to_string(&mut text)?;
        if text.len() > 65_536 {
            return Err(invalid("dotfile exceeds 64 KiB"));
        }
        let after = file.metadata()?;
        let current = std::fs::symlink_metadata(path)?;
        if !current.is_file()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
            || current.len() != after.len()
            || current.modified()? != after.modified()?
        {
            return Err(invalid("dotfile changed during its snapshot read"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.dev() != current.dev()
                || before.ino() != current.ino()
                || after.nlink() != 1
                || current.nlink() != 1
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
            {
                return Err(invalid("dotfile identity changed during its snapshot read"));
            }
        }
        let path = std::fs::canonicalize(path)?;
        let mut config: Self = toml::from_str(&text)?;
        config.validate()?;
        if config.archive.is_relative() {
            config.archive = path
                .parent()
                .ok_or_else(|| invalid("dotfile has no parent"))?
                .join(&config.archive);
        }
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), FriendsError> {
        if self.version != 1 {
            return Err(invalid("unsupported dotfile version"));
        }
        if self.archive.as_os_str().is_empty() {
            return Err(invalid("archive path is empty"));
        }
        bounded("friends", self.friends.len(), 1, 128)?;
        let mut names = BTreeMap::new();
        for (alias, friend) in &self.friends {
            if !valid_handle(alias) || !valid_handle(&friend.handle) {
                return Err(invalid(
                    "friend aliases and handles must use letters, numbers or underscores",
                ));
            }
            if friend
                .label
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 120)
            {
                return Err(invalid("friend labels must contain 1..=120 bytes"));
            }
            if friend
                .account_id
                .as_ref()
                .is_some_and(|s| s.parse::<i64>().map_or(true, |id| id <= 0))
            {
                return Err(invalid("account_id must be a positive Twitter ID"));
            }
            for name in [alias, &friend.handle] {
                if let Some(previous) = names.insert(name.to_ascii_lowercase(), alias)
                    && previous != alias
                {
                    return Err(invalid(format!("ambiguous friend alias @{name}")));
                }
            }
        }
        bounded("max_friends", self.retrieval.max_friends, 1, 8)?;
        bounded(
            "candidates_per_friend",
            self.retrieval.candidates_per_friend,
            8,
            256,
        )?;
        bounded("max_queries", self.retrieval.max_queries, 1, 8)?;
        bounded(
            "retrieval.timeout_seconds",
            self.retrieval.timeout_seconds as usize,
            1,
            120,
        )?;
        bounded(
            "queries",
            self.retrieval.queries.len(),
            0,
            self.retrieval.max_queries,
        )?;
        if self
            .retrieval
            .queries
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err(invalid("queries must contain 1..=256 bytes"));
        }
        if self.retrieval.ignored_terms.len() > 128
            || self
                .retrieval
                .ignored_terms
                .iter()
                .any(|s| s.is_empty() || s.len() > 64 || !s.chars().all(char::is_alphanumeric))
        {
            return Err(invalid("ignored_terms needs at most 128 short words"));
        }
        for date in [&self.retrieval.since, &self.retrieval.until]
            .into_iter()
            .flatten()
        {
            // ISO dates have the same lexical ordering as archive timestamps.
            if date.len() != 10 || chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
                return Err(invalid("since/until must be YYYY-MM-DD dates"));
            }
        }
        if let (Some(since), Some(until)) = (&self.retrieval.since, &self.retrieval.until)
            && since > until
        {
            return Err(invalid("since is later than until"));
        }
        bounded("max_chars", self.prompt.max_chars, 4_000, 128_000)?;
        bounded(
            "max_excerpt_chars",
            self.prompt.max_excerpt_chars,
            100,
            4_000,
        )?;
        bounded(
            "excerpts_per_friend",
            self.prompt.excerpts_per_friend,
            1,
            64,
        )?;
        if self.prompt.direction.trim().is_empty() || self.prompt.direction.len() > 2_000 {
            return Err(invalid("direction must contain 1..=2000 bytes"));
        }
        if self.optimizer.recipes.is_empty()
            || self.optimizer.recipes.len() > 3
            || self.optimizer.recipes.iter().collect::<BTreeSet<_>>().len()
                != self.optimizer.recipes.len()
        {
            return Err(invalid("choose 1..=3 distinct recipes"));
        }
        if let Some(prompting) = &self.prompting {
            prompting.validate()?;
        }
        if let Some(sampling) = &self.sampling {
            sampling.validate(self)?;
        }
        bounded("critique.max_rounds", self.critique.max_rounds, 1, 3)?;
        bounded(
            "critique.max_output_tokens",
            self.critique.max_output_tokens as usize,
            128,
            8_192,
        )?;
        bounded(
            "critique.timeout_seconds",
            self.critique.timeout_seconds as usize,
            1,
            300,
        )?;
        Ok(())
    }
}
