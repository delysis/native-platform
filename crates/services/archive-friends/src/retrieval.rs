use crate::archive::ArchiveDatabase;
use crate::archive::MessageType;
use crate::terms::content_terms;
use crate::{FriendsConfig, FriendsError, fingerprint, invalid};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, UNIX_EPOCH};

// CROSS JOIN is an intentional planner barrier. On the canonical corpus,
// the alternative author-first plan probes FTS once per history row and
// exhausts the interactive deadline, even though only eight matches are needed.
const TOPICAL_SQL: &str = "SELECT raw.rowid FROM messages_fts CROSS JOIN messages raw ON raw.rowid = messages_fts.rowid WHERE messages_fts MATCH ?1 AND raw.owner_account_rowid = ?2 AND raw.kind_id != 4 AND raw.retweeted_tweet_id IS NULL AND (raw.flags & 2) = 0 AND (?3 IS NULL OR raw.created_at >= ?3) AND (?4 IS NULL OR raw.created_at <= ?4) ORDER BY raw.created_at DESC, raw.rowid DESC LIMIT ?5";

#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<(), FriendsError> {
        if self.is_cancelled() {
            Err(invalid("prompt preparation cancelled"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mention {
    pub handle: String,
    /// UTF-8 byte range, including @. Use these to decorate the user's draft.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionQuery {
    pub prefix: String,
    pub start: usize,
    pub end: usize,
}

/// Completion state at a UTF-8 caret offset, including the just-typed bare @.
/// Hosts can drive a keyboard-accessible popover with `suggest(query.prefix)`.
pub fn mention_query(draft: &str, caret: usize) -> Option<MentionQuery> {
    if !draft.is_char_boundary(caret) {
        return None;
    }
    let prefix = &draft[..caret];
    let start = prefix.rfind('@')?;
    if !prefix[start + 1..]
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    // A sentinel makes the existing prose parser handle a bare @ identically.
    let probe = format!("{prefix}x");
    let end = caret
        + draft[caret..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
    mentions(&probe)
        .into_iter()
        .find(|m| m.start == start)
        .map(|_| MentionQuery {
            prefix: prefix[start + 1..].to_string(),
            start,
            end,
        })
}

/// A single author-requested editor transaction, with UTF-8 byte offsets.
/// Apply it through the host editor's transaction API to retain Undo/Redo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    pub caret: usize,
}

pub fn mention_edit(
    config: &FriendsConfig,
    draft: &str,
    caret: usize,
    alias: &str,
) -> Result<MentionEdit, FriendsError> {
    if !config.friends.contains_key(alias) {
        return Err(invalid("completion is not a configured friend"));
    }
    let query = mention_query(draft, caret)
        .ok_or_else(|| invalid("there is no active @ mention at this caret"))?;
    let whitespace = draft[query.end..]
        .chars()
        .next()
        .filter(|c| c.is_whitespace());
    let replacement = if whitespace.is_some() {
        format!("@{alias}")
    } else {
        format!("@{alias} ")
    };
    Ok(MentionEdit {
        start: query.start,
        end: query.end,
        caret: query.start + replacement.len() + whitespace.map(char::len_utf8).unwrap_or(0),
        replacement,
    })
}

/// Convenience for text-only hosts. Interactive editors should apply
/// `mention_edit` as a transaction instead of replacing the entire document.
pub fn insert_mention(
    config: &FriendsConfig,
    draft: &str,
    caret: usize,
    alias: &str,
) -> Result<(String, usize), FriendsError> {
    let edit = mention_edit(config, draft, caret, alias)?;
    let mut updated = draft.to_string();
    updated.replace_range(edit.start..edit.end, &edit.replacement);
    Ok((updated, edit.caret))
}

/// Mentions in prose only: skip emails, URLs, escaped @, inline and fenced code.
pub fn mentions(text: &str) -> Vec<Mention> {
    let mut found = Vec::new();
    let mut code_ticks = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((start, ch)) = iter.next() {
        if ch == '`' {
            let mut ticks = 1;
            while iter.peek().is_some_and(|(_, c)| *c == '`') {
                iter.next();
                ticks += 1;
            }
            if code_ticks == 0 {
                code_ticks = ticks;
            } else if ticks == code_ticks {
                code_ticks = 0;
            }
            continue;
        }
        if ch != '@' || code_ticks != 0 {
            continue;
        }
        if text[..start].chars().next_back().is_some_and(|c| {
            c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '+' | '-' | '@' | '\\')
        }) {
            continue;
        }
        let mut end = start + 1;
        while let Some(&(i, c)) = iter.peek() {
            if !c.is_ascii_alphanumeric() && c != '_' {
                break;
            }
            end = i + c.len_utf8();
            iter.next();
        }
        let handle = &text[start + 1..end];
        if !handle.is_empty() {
            found.push(Mention {
                handle: handle.to_ascii_lowercase(),
                start,
                end,
            });
        }
    }
    found
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FriendSuggestion {
    pub alias: String,
    pub handle: String,
    pub label: String,
}

/// Fast, filesystem-free completion over the explicitly configured circle.
pub fn suggest(config: &FriendsConfig, prefix: &str) -> Vec<FriendSuggestion> {
    let prefix = prefix.trim_start_matches('@').to_ascii_lowercase();
    config
        .friends
        .iter()
        .filter(|(alias, f)| {
            alias.to_ascii_lowercase().starts_with(&prefix)
                || f.handle.to_ascii_lowercase().starts_with(&prefix)
                || f.label
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().starts_with(&prefix))
        })
        .take(12)
        .map(|(alias, f)| FriendSuggestion {
            alias: alias.clone(),
            handle: f.handle.clone(),
            label: f.label.clone().unwrap_or_else(|| f.handle.clone()),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Friend {
    pub alias: String,
    pub handle: String,
    pub account_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub id: String,
    pub account_id: String,
    pub handle: String,
    pub created_at: Option<String>,
    pub thread_id: Option<String>,
    pub reply_to_tweet_id: Option<String>,
    pub quoted_tweet_id: Option<String>,
    pub source_url: Option<String>,
    pub source_collection: String,
    pub archive_path: String,
    pub text: String,
    pub full_text_sha256: String,
    pub truncated: bool,
    pub matched_queries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<EvidenceSignals>,
}

/// Archive facts, not quality labels. Engagement is only a noisy proxy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceSignals {
    pub reply_to_user_id: Option<String>,
    pub reply_to_username: Option<String>,
    pub full_text_chars: usize,
    pub favorite_count: Option<i64>,
    pub retweet_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveStamp {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified_unix_nanos: u128,
}

fn stamp(path: &Path) -> Result<ArchiveStamp, FriendsError> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(invalid("archive must be a file"));
    }
    Ok(ArchiveStamp {
        path: path.to_path_buf(),
        bytes: meta.len(),
        modified_unix_nanos: meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("invalid archive modification time"))?
            .as_nanos(),
    })
}

pub(crate) fn check_wal(path: &Path) -> Result<(), FriendsError> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    match std::fs::metadata(PathBuf::from(wal)) {
        Ok(m) if m.len() > 0 => Err(invalid(
            "archive has an active WAL; select a checkpointed immutable snapshot",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedCircle {
    pub archive: ArchiveStamp,
    pub config_sha256: String,
    pub draft: String,
    pub mentions: Vec<Mention>,
    pub friends: Vec<Friend>,
    pub queries: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub notices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_friends: Vec<Friend>,
}

pub fn retrieve(
    config: &FriendsConfig,
    draft: &str,
    cancel: &Cancellation,
) -> Result<RetrievedCircle, FriendsError> {
    retrieve_with_mentions(config, draft, cancel, mentions(draft))
}

/// Native preparation has already selected invitations with the shared
/// workspace grammar. Its synthesized first line contains only those handles;
/// the preserved query text below it cannot acquire participant authority.
pub(crate) fn retrieve_prepared_invitation(
    config: &FriendsConfig,
    draft: &str,
    cancel: &Cancellation,
) -> Result<RetrievedCircle, FriendsError> {
    let (invited, _) = draft
        .split_once('\n')
        .ok_or_else(|| invalid("native invitation header is missing"))?;
    retrieve_with_mentions(config, draft, cancel, mentions(invited))
}

fn retrieve_with_mentions(
    config: &FriendsConfig,
    draft: &str,
    cancel: &Cancellation,
    mentions: Vec<Mention>,
) -> Result<RetrievedCircle, FriendsError> {
    config.validate()?;
    cancel.check()?;
    if draft.trim().is_empty() || draft.len() > 16_384 {
        return Err(invalid("draft must contain 1..=16384 UTF-8 bytes"));
    }
    if mentions.is_empty() {
        return Err(invalid("mention a configured friend, for example @visa"));
    }
    let mut selected = Vec::new();
    let mut aliases = BTreeSet::new();
    for m in &mentions {
        let (alias, f) = config
            .friends
            .iter()
            .find(|(alias, f)| {
                alias.eq_ignore_ascii_case(&m.handle) || f.handle.eq_ignore_ascii_case(&m.handle)
            })
            .ok_or_else(|| invalid(format!("@{} is not in your dotfile's friends", m.handle)))?;
        if aliases.insert(alias.clone()) {
            selected.push((alias, f));
        }
    }
    if selected.len() > config.retrieval.max_friends {
        return Err(invalid(format!(
            "this circle allows at most {} friends",
            config.retrieval.max_friends
        )));
    }
    let target_aliases = aliases.clone();
    if let Some(sampling) = &config.sampling {
        for alias in &sampling.context {
            if aliases.insert(alias.clone()) {
                selected.push((alias, &config.friends[alias]));
            }
        }
    }
    let path = std::fs::canonicalize(&config.archive)?;
    check_wal(&path)?;
    let archive = stamp(&path)?;
    let db = ArchiveDatabase::open_immutable_read_only(&path)?;
    let stop = cancel.clone();
    let started = Instant::now();
    // A bounded candidate count alone is not a query deadline on a large corpus.
    let timeout = Duration::from_secs(config.retrieval.timeout_seconds);
    db.connection().progress_handler(
        2_000,
        Some(move || stop.is_cancelled() || started.elapsed() > timeout),
    )?;
    let mut topic = draft.to_string();
    for m in mentions.iter().rev() {
        topic.replace_range(m.start..m.end, " ");
    }
    let queries = if config.retrieval.queries.is_empty() {
        content_terms(&topic, 128)
            .into_iter()
            .filter(|term| {
                !config
                    .retrieval
                    .ignored_terms
                    .iter()
                    .any(|ignored| ignored.eq_ignore_ascii_case(term))
            })
            .take(config.retrieval.max_queries)
            .collect()
    } else {
        config.retrieval.queries.clone()
    };
    let mut friends = Vec::new();
    let mut context_friends = Vec::new();
    let mut evidence = Vec::new();
    let mut notices = Vec::new();
    let mut account_ids = BTreeSet::new();
    for (alias, f) in selected {
        cancel.check()?;
        // Avoid AccountLookup's corpus-wide aggregate counts on the interactive path.
        let row: Option<(i64, String, String, Option<String>)> = if let Some(id) = &f.account_id {
            db.connection().query_row("SELECT account_rowid, CAST(twitter_id AS TEXT), username, display_name FROM accounts WHERE twitter_id = CAST(? AS INTEGER)", params![id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?
        } else {
            let mut stmt = db.connection().prepare("SELECT account_rowid, CAST(twitter_id AS TEXT), username, display_name FROM accounts WHERE username = ? COLLATE NOCASE LIMIT 2")?;
            let rows = stmt
                .query_map(params![f.handle], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if rows.len() > 1 {
                return Err(invalid(format!(
                    "@{} is ambiguous; pin account_id in the dotfile",
                    f.handle
                )));
            }
            rows.into_iter().next()
        };
        let (rowid, account_id, handle, display_name) =
            row.ok_or_else(|| invalid(format!("@{} was not found in this archive", f.handle)))?;
        if !account_ids.insert(account_id.clone()) {
            continue;
        }
        let friend = Friend {
            alias: alias.clone(),
            handle: handle.clone(),
            account_id: account_id.clone(),
            label: f
                .label
                .clone()
                .or(display_name)
                .unwrap_or_else(|| handle.clone()),
        };
        if target_aliases.contains(alias) {
            friends.push(friend);
        } else {
            context_friends.push(friend);
        }
        let mut by_id: BTreeMap<String, Evidence> = BTreeMap::new();
        let mut passes: Vec<Option<&str>> = queries.iter().map(|q| Some(q.as_str())).collect();
        if config.retrieval.include_recent {
            passes.push(None);
        }
        let extra = if config.sampling.is_some() { 4 } else { 0 };
        let per_pass =
            (config.retrieval.candidates_per_friend / (passes.len() + extra).max(1)).max(1);
        for query in passes {
            cancel.check()?;
            let until = config
                .retrieval
                .until
                .as_ref()
                .map(|d| format!("{d}T23:59:59.999999Z"));
            let messages = if let Some(query) = query {
                // Intersect the author and topic posting lists inside FTS5.
                // Filtering a global topic result through the timeline join is
                // unbounded work on the canonical 100+ GiB archive.
                let terms = query
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                if terms.is_empty() {
                    continue;
                }
                let fts = format!(
                    "username: \"{}\" AND text: ({terms})",
                    handle.replace('"', "\"\"")
                );
                let mut statement = db.connection().prepare(TOPICAL_SQL)?;
                let ids = statement
                    .query_map(
                        params![fts, rowid, config.retrieval.since, until, per_pass as i64],
                        |r| r.get::<_, i64>(0),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                ids.into_iter()
                    .map(|id| db.message_by_rowid(id))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            } else {
                recent_authored(&db, rowid, per_pass, config)?
            };
            for m in messages {
                if m.account_id.as_deref() != Some(&account_id)
                    || m.message_type == MessageType::LikedTweet
                    || m.is_retweet
                    || m.text.trim().is_empty()
                {
                    continue;
                }
                if let Some(existing) = by_id.get_mut(&m.message_id) {
                    if let Some(q) = query
                        && !existing.matched_queries.iter().any(|s| s == q)
                    {
                        existing.matched_queries.push(q.to_string());
                    }
                    continue;
                }
                if by_id.len() >= config.retrieval.candidates_per_friend {
                    continue;
                }
                by_id.insert(
                    m.message_id.clone(),
                    evidence_from_message(m, &account_id, &handle, config, &queries),
                );
            }
        }
        if let Some(sampling) = &config.sampling {
            // Bound the author scan before ranking. These are the longest / most
            // engaged in this window, not a claim about the entire archive.
            let pool: Vec<_> = recent_authored(&db, rowid, sampling.pool_window, config)?
                .into_iter()
                .filter(|m| {
                    m.account_id.as_deref() == Some(&account_id)
                        && m.message_type != MessageType::LikedTweet
                        && !m.is_retweet
                        && !m.text.trim().is_empty()
                })
                .map(|m| evidence_from_message(m, &account_id, &handle, config, &queries))
                .collect();
            for axis in 0..4 {
                cancel.check()?;
                let mut ranked: Vec<_> = pool.iter().collect();
                ranked.sort_by(|a, b| {
                    let score = |e: &Evidence| match axis {
                        0 => f64::from(crate::reply_to_other(e)),
                        1 => e.text.chars().count() as f64,
                        2 => e
                            .signals
                            .as_ref()
                            .and_then(|s| s.favorite_count)
                            .unwrap_or(0)
                            .max(0) as f64,
                        _ => 0.0,
                    };
                    score(b)
                        .total_cmp(&score(a))
                        .then_with(|| b.created_at.cmp(&a.created_at))
                        .then_with(|| a.id.cmp(&b.id))
                });
                for e in ranked.into_iter().take(per_pass) {
                    by_id.entry(e.id.clone()).or_insert_with(|| e.clone());
                }
            }
        }
        if by_id.is_empty() {
            notices.push(format!(
                "@{handle}: no authored evidence in the selected archive/date window."
            ));
        } else if by_id.values().all(|e| e.matched_queries.is_empty()) {
            notices.push(format!(
                "@{handle}: no topical matches; only background authored material is available."
            ));
        }
        evidence.extend(by_id.into_values());
    }
    check_wal(&path)?;
    if stamp(&path)? != archive {
        return Err(invalid(
            "archive changed during retrieval; retry with an immutable snapshot",
        ));
    }
    cancel.check()?;
    if evidence.is_empty() {
        return Err(invalid("no authored evidence found for this circle"));
    }
    Ok(RetrievedCircle {
        archive,
        config_sha256: fingerprint(config)?,
        draft: draft.into(),
        mentions,
        friends,
        context_friends,
        queries,
        evidence,
        notices,
    })
}

/// Limit the indexed author scan before hydrating compatibility-view records.
/// A self-join before ORDER BY makes SQLite sort the author's entire history,
/// defeating the recent-author index on large archives.
fn recent_authored(
    db: &ArchiveDatabase,
    account_rowid: i64,
    limit: usize,
    config: &FriendsConfig,
) -> Result<Vec<crate::archive::Message>, FriendsError> {
    let until = config
        .retrieval
        .until
        .as_ref()
        .map(|d| format!("{d}T23:59:59.999999Z"));
    let mut statement = db.connection().prepare(
        "SELECT rowid FROM messages WHERE owner_account_rowid = ?1 AND kind_id != 4 AND retweeted_tweet_id IS NULL AND (flags & 2) = 0 AND (?2 IS NULL OR created_at >= ?2) AND (?3 IS NULL OR created_at <= ?3) ORDER BY created_at DESC, tweet_id DESC, rowid DESC LIMIT ?4",
    )?;
    let ids = statement
        .query_map(
            params![account_rowid, config.retrieval.since, until, limit as i64],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| db.message_by_rowid(id))
        .collect::<Result<Vec<_>, _>>()
        .map(|messages| messages.into_iter().flatten().collect())
}

fn evidence_from_message(
    m: crate::archive::Message,
    account_id: &str,
    handle: &str,
    config: &FriendsConfig,
    queries: &[String],
) -> Evidence {
    let words: BTreeSet<_> = m
        .text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    let matched_queries = queries
        .iter()
        .filter(|q| {
            let terms: Vec<_> = q
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|s| !s.is_empty())
                .collect();
            !terms.is_empty() && terms.iter().all(|s| words.contains(&s.to_lowercase()))
        })
        .cloned()
        .collect();
    let text: String = m
        .text
        .chars()
        .take(config.prompt.max_excerpt_chars)
        .collect();
    Evidence {
        id: m.message_id,
        account_id: account_id.into(),
        handle: handle.into(),
        created_at: m.created_at,
        thread_id: m.thread_id,
        reply_to_tweet_id: m.reply_to_tweet_id,
        quoted_tweet_id: m.quoted_tweet_id,
        source_url: m
            .tweet_id
            .as_ref()
            .map(|id| format!("https://x.com/{handle}/status/{id}")),
        source_collection: m.source_collection,
        archive_path: m.archive_path,
        truncated: text.len() < m.text.len(),
        text,
        full_text_sha256: format!("{:x}", Sha256::digest(m.text.as_bytes())),
        matched_queries,
        signals: Some(EvidenceSignals {
            reply_to_user_id: m.reply_to_user_id,
            reply_to_username: m.reply_to_username,
            full_text_chars: m.text.chars().count(),
            favorite_count: m.favorite_count,
            retweet_count: m.retweet_count,
        }),
    }
}

#[cfg(test)]
mod query_plan_tests {
    use super::*;

    #[test]
    fn topical_search_drives_from_fts_before_hydrating_author_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE messages (owner_account_rowid INTEGER, kind_id INTEGER, retweeted_tweet_id INTEGER, flags INTEGER, created_at TEXT); CREATE INDEX owner_recent ON messages(owner_account_rowid, created_at DESC) WHERE kind_id != 4; CREATE VIRTUAL TABLE messages_fts USING fts5(username, text);")?;
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {TOPICAL_SQL}"))?
            .query_map(
                params![
                    "username: alice AND text: friendship",
                    1,
                    None::<String>,
                    None::<String>,
                    8
                ],
                |row| row.get::<_, String>(3),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            plan.first()
                .is_some_and(|step| step.contains("messages_fts VIRTUAL TABLE")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("raw USING INTEGER PRIMARY KEY")),
            "{plan:?}"
        );
        Ok(())
    }
}
