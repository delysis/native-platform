//! Verifies observations emitted after successful native decode calls in the
//! instrumented qualification build. Parsing fixtures never creates such proof.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decode {
    kind: String,
    request_id: String,
    case_ids: Vec<String>,
    sequence_ids: Vec<i32>,
    generated_counts: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub shared_decode_line: usize,
    pub survivor_decode_line: usize,
    pub survivor_generated_count: usize,
}

fn records(text: &str) -> Result<Vec<Decode>, String> {
    if text.len() > 8 * 1024 * 1024 {
        return Err("native decode trace exceeds its qualification limit".into());
    }
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let item: Decode = serde_json::from_str(line).map_err(|error| error.to_string())?;
            if item.kind != "loom_native_decode_batch_v1"
                || item.request_id.is_empty()
                || item.case_ids.is_empty()
                || item.case_ids.len() > 4
                || item.case_ids.len() != item.sequence_ids.len()
                || item.case_ids.len() != item.generated_counts.len()
                || item.case_ids.iter().collect::<BTreeSet<_>>().len() != item.case_ids.len()
                || item.sequence_ids.iter().collect::<BTreeSet<_>>().len() != item.case_ids.len()
                || item.sequence_ids.iter().any(|id| *id < 0)
                || item.generated_counts.contains(&0)
            {
                return Err("malformed native decode observation".into());
            }
            Ok(item)
        })
        .collect()
}

pub fn find_shared(text: &str, first: &str, second: &str) -> Result<Option<String>, String> {
    if first == second {
        return Err("qualification requires independent case identities".into());
    }
    Ok(records(text)?
        .into_iter()
        .find(|item| {
            item.case_ids.len() == 2
                && item.case_ids.iter().any(|id| id == first)
                && item.case_ids.iter().any(|id| id == second)
        })
        .map(|item| item.request_id))
}

pub fn verify(text: &str, batch: &str, cancelled: &str, survivor: &str) -> Result<Summary, String> {
    if cancelled == survivor {
        return Err("qualification requires two distinct case identities".into());
    }
    let records = records(text)?;
    let mut shared = None;
    let mut surviving = None;
    let mut survivor_sequence = None;
    let mut survivor_count = 0;
    for (line, item) in records.iter().enumerate() {
        if item.request_id != batch {
            continue;
        }
        if item.case_ids.iter().any(|id| id != cancelled && id != survivor) {
            return Err("unrelated case entered the qualified native batch".into());
        }
        if surviving.is_some() && item.case_ids.iter().any(|id| id == cancelled) {
            return Err("a cancelled sequence re-entered native decoding".into());
        }
        let Some(index) = item.case_ids.iter().position(|id| id == survivor) else {
            continue;
        };
        let sequence = item.sequence_ids[index];
        let count = item.generated_counts[index];
        if survivor_sequence.is_some_and(|previous| previous != sequence)
            || count <= survivor_count
        {
            return Err("survivor native sequence changed or stopped progressing".into());
        }
        survivor_sequence = Some(sequence);
        survivor_count = count;
        if item.case_ids.len() == 2 {
            shared.get_or_insert(line + 1);
        } else if shared.is_some() {
            surviving.get_or_insert(line + 1);
        }
    }
    Ok(Summary {
        shared_decode_line: shared.ok_or("no successful shared native decode was observed")?,
        survivor_decode_line: surviving.ok_or("no surviving sequence decoded after cancellation")?,
        survivor_generated_count: survivor_count,
    })
}
