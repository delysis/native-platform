use crate::consult::{ConsultPanel, ConsultPersona};
use serde::Deserialize;

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

pub(crate) fn builtin_personas() -> Vec<ConsultPersona> {
    source_personas()
        .into_iter()
        .map(|source| ConsultPersona {
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

// The former default panels are retained only for the legacy consult CLI. The
// chat-native product does not seed them as Persona groups; groups are created
// and ordered by the user in Settings.
pub(crate) fn builtin_panels() -> Vec<ConsultPanel> {
    let personas = builtin_personas();
    let get = |id: &str| {
        personas
            .iter()
            .find(|persona| persona.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("built-in consult Persona `{id}` is missing"))
    };
    vec![
        panel(
            "builtin-trauma-balanced",
            "Balanced trauma consultation",
            vec![
                get("judith_herman"),
                get("peter_levine"),
                get("richard_schwartz"),
                get("ad_de_jongh"),
            ],
        ),
        panel(
            "builtin-complex-trauma",
            "Developmental & complex trauma",
            vec![
                get("bessel_van_der_kolk"),
                get("janina_fisher"),
                get("christine_courtois"),
                get("dolores_mosquera"),
            ],
        ),
        panel(
            "builtin-emdr-formulation",
            "EMDR case formulation",
            vec![
                get("francine_shapiro"),
                get("ad_de_jongh"),
                get("jim_knipe"),
                get("dolores_mosquera"),
            ],
        ),
        panel(
            "builtin-compulsion-recovery",
            "Compulsion & recovery",
            vec![
                get("gabor_mate"),
                get("robert_miller_fsap"),
                get("arnold_popky_detur"),
                get("shirley_jean_schmidt_dnms"),
            ],
        ),
    ]
}

fn panel(id: &str, name: &str, personas: Vec<ConsultPersona>) -> ConsultPanel {
    ConsultPanel {
        id: id.to_string(),
        name: name.to_string(),
        personas,
        created_at: LIBRARY_REVISION.to_string(),
        updated_at: LIBRARY_REVISION.to_string(),
    }
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

    #[test]
    fn legacy_panels_are_bounded_but_are_not_the_seeded_persona_groups() {
        let panels = builtin_panels();
        assert!(
            panels
                .iter()
                .all(|panel| !panel.personas.is_empty() && panel.personas.len() <= 4)
        );
        assert!(panels.iter().all(|panel| panel.id.starts_with("builtin-")));
    }
}
