use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type ModelId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScores {
    pub general: f64,
    pub coding: f64,
    pub reasoning: f64,
    pub creative: f64,
    pub multilingual: f64,
    pub speed: f64,
}

impl Default for TaskScores {
    fn default() -> Self {
        Self {
            general: 0.5,
            coding: 0.5,
            reasoning: 0.5,
            creative: 0.5,
            multilingual: 0.5,
            speed: 0.5,
        }
    }
}

pub struct EvalStore {
    scores: Arc<Mutex<HashMap<ModelId, TaskScores>>>,
}

impl Default for EvalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EvalStore {
    pub fn new() -> Self {
        Self {
            // A neutral score is returned until a real evaluation result is
            // ingested. Invented benchmark values must never influence routes.
            scores: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_score(&self, model: &str) -> TaskScores {
        let scores = self
            .scores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scores.get(model).cloned().unwrap_or_default()
    }

    pub fn update_scores(&self, new_scores: HashMap<ModelId, TaskScores>) {
        let mut scores = self
            .scores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scores.extend(
            new_scores
                .into_iter()
                .map(|(model, score)| (model, score.normalized())),
        );
    }
}

impl TaskScores {
    fn normalized(self) -> Self {
        Self {
            general: normalize(self.general),
            coding: normalize(self.coding),
            reasoning: normalize(self.reasoning),
            creative: normalize(self.creative),
            multilingual: normalize(self.multilingual),
            speed: normalize(self.speed),
        }
    }
}

fn normalize(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingested_scores_are_finite_and_normalized() {
        let store = EvalStore::new();
        store.update_scores(HashMap::from([(
            "model".to_string(),
            TaskScores {
                general: f64::NAN,
                coding: 2.0,
                reasoning: -1.0,
                creative: 0.8,
                multilingual: 0.7,
                speed: f64::INFINITY,
            },
        )]));

        let score = store.get_score("model");
        assert_eq!(score.general, 0.5);
        assert_eq!(score.coding, 1.0);
        assert_eq!(score.reasoning, 0.0);
        assert_eq!(score.creative, 0.8);
        assert_eq!(score.multilingual, 0.7);
        assert_eq!(score.speed, 0.5);
    }
}
