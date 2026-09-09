use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BuiltinPersona {
    pub id: String,
    pub label: String,
    pub description: String,
    pub perspective_prompt: String,
    #[serde(default)]
    pub public_figure: Option<String>,
    #[serde(default)]
    pub expertise: Option<String>,
    #[serde(default)]
    pub model_slot: Option<usize>,
}

pub(crate) const LIBRARY_REVISION: &str = "therapy-consult-personas-09557b34-2026-08-03";

#[derive(Debug, Deserialize)]
struct SourcePersona {
    id: String,
    name: String,
    modality: String,
    system_prompt: String,
}

fn source_personas() -> Vec<SourcePersona> {
    match serde_yaml::from_str(include_str!("../assets/therapy_consult_personas.yaml")) {
        Ok(personas) => personas,
        Err(error) => panic!("the checked-in therapy Persona catalog is invalid: {error}"),
    }
}

pub(crate) fn builtin_personas() -> Vec<BuiltinPersona> {
    source_personas()
        .into_iter()
        .map(|source| BuiltinPersona {
            id: source.id,
            label: source.name.clone(),
            description: source.modality.clone(),
            perspective_prompt: source.system_prompt,
            public_figure: Some(source.name),
            expertise: Some(source.modality),
            model_slot: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn catalog_is_the_exact_supplied_fourteen_persona_library() {
        let personas = builtin_personas();
        assert_eq!(personas.len(), 14);
        assert_eq!(
            personas
                .iter()
                .map(|persona| persona.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            personas.len()
        );
        assert_eq!(personas[0].label, "Bessel van der Kolk");
        assert_eq!(personas[1].label, "Gabor Maté");
        assert_eq!(personas[13].label, "Dolores Mosquera");
        assert!(personas.iter().all(|persona| {
            !persona.label.ends_with("lens")
                && !persona.perspective_prompt.trim().is_empty()
                && persona
                    .perspective_prompt
                    .contains(&format!("You are specifically modeling: {}", persona.label))
        }));
    }
}
