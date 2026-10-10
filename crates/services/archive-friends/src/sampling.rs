use crate::{
    Evidence, FriendsConfig, FriendsError, PromptPack, Recipe, RetrievedCircle,
    compiler::{evidence_block, render_prompt},
    fingerprint, invalid,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SamplingSettings {
    pub active: String,
    /// Extra configured authors, e.g. the writer. Never inferred from the host.
    pub context: Vec<String>,
    /// Recent authored rows examined for the reply/length/engagement strata.
    pub pool_window: usize,
    pub max_context_fraction: f64,
    pub policies: BTreeMap<String, SamplingPolicy>,
}

impl Default for SamplingSettings {
    fn default() -> Self {
        let balanced = SamplingPolicy::default();
        Self {
            active: "balanced".into(),
            context: Vec::new(),
            pool_window: 1024,
            max_context_fraction: 0.25,
            policies: BTreeMap::from([
                ("balanced".into(), balanced.clone()),
                (
                    "conversation".into(),
                    SamplingPolicy {
                        reply: 6.0,
                        context: 2.0,
                        ..balanced.clone()
                    },
                ),
                (
                    "longform".into(),
                    SamplingPolicy {
                        length: 6.0,
                        recency: 0.5,
                        ..balanced.clone()
                    },
                ),
                (
                    "recent".into(),
                    SamplingPolicy {
                        recency: 6.0,
                        ..balanced
                    },
                ),
            ]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SamplingPolicy {
    pub topical: f64,
    pub reply: f64,
    pub length: f64,
    pub engagement: f64,
    pub recency: f64,
    pub target: f64,
    pub context: f64,
    pub novelty: f64,
    pub half_life_days: f64,
}
impl Default for SamplingPolicy {
    fn default() -> Self {
        Self {
            topical: 6.0,
            reply: 2.0,
            length: 1.0,
            engagement: 0.25,
            recency: 1.0,
            target: 2.0,
            context: 0.5,
            novelty: 1.0,
            half_life_days: 365.0,
        }
    }
}
impl SamplingSettings {
    pub(crate) fn validate(&self, config: &FriendsConfig) -> Result<(), FriendsError> {
        if !(32..=4096).contains(&self.pool_window)
            || self.policies.is_empty()
            || self.policies.len() > 16
            || !self.policies.contains_key(&self.active)
            || self.context.len() > 4
            || self.context.iter().collect::<BTreeSet<_>>().len() != self.context.len()
            || self.context.iter().any(|a| !config.friends.contains_key(a))
            || !self.max_context_fraction.is_finite()
            || !(0.0..=0.5).contains(&self.max_context_fraction)
        {
            return Err(invalid(
                "invalid sampling policy, context authors, pool window or context fraction",
            ));
        }
        for (name, p) in &self.policies {
            if name.is_empty()
                || name.len() > 48
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || [
                    p.topical,
                    p.reply,
                    p.length,
                    p.engagement,
                    p.recency,
                    p.target,
                    p.context,
                    p.novelty,
                ]
                .iter()
                .any(|w| !w.is_finite() || !(0.0..=20.0).contains(w))
                || !p.half_life_days.is_finite()
                || !(1.0..=36500.0).contains(&p.half_life_days)
            {
                return Err(invalid(
                    "sampling weights must be finite in 0..=20; half_life_days in 1..=36500",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingReceipt {
    pub policy: String,
    pub weights: SamplingPolicy,
    pub reference_date: Option<String>,
    pub selected: Vec<SelectionReason>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionReason {
    pub evidence_id: String,
    pub score: f64,
    pub topical: f64,
    pub reply_to_other: bool,
    pub length: f64,
    pub engagement: f64,
    pub recency: f64,
    pub is_target: bool,
    pub new_thread: bool,
}

pub fn reply_to_other(e: &Evidence) -> bool {
    if e.reply_to_tweet_id.is_none() {
        return false;
    }
    let Some(s) = &e.signals else {
        return false;
    };
    if let Some(id) = &s.reply_to_user_id {
        return id != &e.account_id;
    }
    s.reply_to_username
        .as_ref()
        .is_some_and(|h| !h.eq_ignore_ascii_case(&e.handle))
}
fn date(e: &Evidence) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(e.created_at.as_deref()?.get(..10)?, "%Y-%m-%d").ok()
}

/// Compile a named hypothesis against the same frozen candidate pool. All arms
/// use the same writing instructions, character budget and attribution rules.
pub fn compile_sample(
    config: &FriendsConfig,
    circle: RetrievedCircle,
    policy: &str,
) -> Result<PromptPack, FriendsError> {
    config.validate()?;
    if circle.config_sha256 != fingerprint(config)? {
        return Err(invalid("retrieval configuration changed before sampling"));
    }
    let settings = config
        .sampling
        .as_ref()
        .ok_or_else(|| invalid("configure [sampling] first"))?;
    let weights = settings
        .policies
        .get(policy)
        .ok_or_else(|| invalid("unknown sampling policy"))?;
    let targets: BTreeSet<_> = circle
        .friends
        .iter()
        .map(|f| f.account_id.as_str())
        .collect();
    let reference = circle.evidence.iter().filter_map(date).max();
    let mut ids = Vec::new();
    let mut reasons = Vec::new();
    let mut selected: Vec<&Evidence> = Vec::new();
    let mut chars = render_prompt(config, &circle, Recipe::Resonance, &[])?
        .chars()
        .count();
    let mut context_chars = 0;
    let evidence_budget = config
        .prompt
        .max_chars
        .checked_sub(chars)
        .ok_or_else(|| invalid("draft exceeds prompt budget"))?;
    let context_budget = (evidence_budget as f64 * settings.max_context_fraction) as usize;
    let mut rejected = BTreeSet::new();
    loop {
        // Reserve a turn for every target before optional background authors.
        let missing = circle.friends.iter().find(|f| {
            circle.evidence.iter().any(|e| e.account_id == f.account_id)
                && !selected.iter().any(|e| e.account_id == f.account_id)
        });
        let mut choices: Vec<_> = circle
            .evidence
            .iter()
            .filter(|e| {
                !rejected.contains(&e.id)
                    && !ids.contains(&e.id)
                    && missing.is_none_or(|f| e.account_id == f.account_id)
                    && selected
                        .iter()
                        .filter(|s| s.account_id == e.account_id)
                        .count()
                        < config.prompt.excerpts_per_friend
                    && !selected.iter().any(|s| {
                        s.account_id == e.account_id && s.full_text_sha256 == e.full_text_sha256
                    })
            })
            .map(|e| {
                let topical = e.matched_queries.len() as f64 / circle.queries.len().max(1) as f64;
                let reply = reply_to_other(e);
                // Reward material actually visible to the model, not a truncated tail.
                let length = (e.text.chars().count() as f64).ln_1p()
                    / (config.prompt.max_excerpt_chars as f64).ln_1p();
                let engagement = (e
                    .signals
                    .as_ref()
                    .and_then(|s| s.favorite_count)
                    .unwrap_or(0)
                    .max(0) as f64)
                    .ln_1p()
                    .min(10.0)
                    / 10.0;
                let recency = reference.zip(date(e)).map_or(0.0, |(r, d)| {
                    2.0_f64.powf(-((r - d).num_days().max(0) as f64) / weights.half_life_days)
                });
                let is_target = targets.contains(e.account_id.as_str());
                let new_thread = !selected.iter().any(|s| {
                    s.thread_id.as_deref().unwrap_or(&s.id)
                        == e.thread_id.as_deref().unwrap_or(&e.id)
                });
                let score = weights.topical * topical
                    + weights.reply * f64::from(reply)
                    + weights.length * length
                    + weights.engagement * engagement
                    + weights.recency * recency
                    + if is_target {
                        weights.target
                    } else {
                        weights.context
                    }
                    + weights.novelty * f64::from(new_thread);
                (
                    e,
                    SelectionReason {
                        evidence_id: e.id.clone(),
                        score,
                        topical,
                        reply_to_other: reply,
                        length,
                        engagement,
                        recency,
                        is_target,
                        new_thread,
                    },
                )
            })
            .collect();
        choices.sort_by(|(a, sa), (b, sb)| {
            sb.score.total_cmp(&sa.score).then_with(|| a.id.cmp(&b.id))
        });
        let mut added = false;
        for (e, reason) in choices {
            let size = evidence_block(e)?.chars().count();
            if chars + size > config.prompt.max_chars
                || (!reason.is_target && context_chars + size > context_budget)
            {
                rejected.insert(e.id.clone());
                continue;
            }
            if !reason.is_target {
                context_chars += size;
            }
            chars += size;
            ids.push(e.id.clone());
            selected.push(e);
            reasons.push(reason);
            added = true;
            break;
        }
        if !added {
            if let Some(f) = missing {
                return Err(invalid(format!(
                    "prompt budget cannot represent @{}",
                    f.handle
                )));
            }
            break;
        }
    }
    if ids.is_empty() {
        return Err(invalid("no evidence fits sampling budget"));
    }
    let prompt = render_prompt(config, &circle, Recipe::Resonance, &ids)?;
    let mut pack = PromptPack {
        schema: "community-archive.friends-prompt.v1".into(),
        id: String::new(),
        notices: circle.notices.clone(),
        circle,
        recipe_trials: Vec::new(),
        selected_recipe: Recipe::Resonance,
        evidence_ids: ids,
        prompt_chars: prompt.chars().count(),
        prompt,
        critique: Vec::new(),
        sampling: Some(SamplingReceipt {
            policy: policy.into(),
            weights: weights.clone(),
            reference_date: reference.map(|d| d.to_string()),
            selected: reasons,
        }),
    };
    pack.seal()?;
    Ok(pack)
}
