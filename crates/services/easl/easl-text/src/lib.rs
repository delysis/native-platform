#![forbid(unsafe_code)]
//! EASL text algorithms with native shaping and drawing primitives.
mod editing;
mod hit_testing;
mod navigation;
mod painting;
mod runtime;
mod styling;
mod viewport;
use easl_native_text::{Alignment, BreakOpportunity, LineStats, PreparedText};
pub use editing::{EDITING_SOURCE, EditAction, EditPlan, Editing};
pub use hit_testing::HitTesting;
pub use navigation::{LineMovement, Navigation};
pub use painting::GlyphPainting;
pub use styling::{STYLES_SOURCE, StyleInput, StyleProperties, StyleRule, Styling};
pub use viewport::Viewport;

pub const PARAGRAPH_SOURCE: &str = include_str!("../library/paragraph.easl");
const NATIVE_SOURCE: &str = include_str!("../library/native.easl");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Native(#[from] easl_native_text::Error),
    #[error("EASL text runtime: {0}")]
    Language(String),
    #[error("Invalid text input or result")]
    Invalid,
    #[error("No fitting paragraph composition")]
    Unbreakable,
    #[error("Text operation exceeded its work bound")]
    Limit,
    #[error("Text runtime faulted; construct a new instance")]
    Faulted,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub hyphen_penalty: f32,
    pub consecutive_hyphen_penalty: f32,
    pub last_line_weight: f32,
    pub edge_limit: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            hyphen_penalty: 0.05,
            consecutive_hyphen_penalty: 0.2,
            last_line_weight: 0.,
            edge_limit: 1_000_000,
        }
    }
}

pub struct Composer {
    runtime: runtime::Runtime,
    faulted: bool,
    pub compositions: u64,
}
impl std::fmt::Debug for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composer")
            .field("faulted", &self.faulted)
            .field("compositions", &self.compositions)
            .finish_non_exhaustive()
    }
}
impl Composer {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            runtime::Runtime::compile(&[
                ("native.easl", NATIVE_SOURCE),
                ("paragraph.easl", PARAGRAPH_SOURCE),
            ])
        })
        .map_err(|_| Error::Language("Compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
            compositions: 0,
        })
    }

    pub fn compose(
        &mut self,
        text: &mut PreparedText,
        width: f32,
        alignment: Alignment,
        policy: Policy,
    ) -> Result<LineStats, Error> {
        text.compose_with(width, alignment, |points, width| {
            self.choose_breaks(points, width, policy)
        })
    }

    pub fn choose_breaks(
        &mut self,
        points: &[BreakOpportunity],
        width: f32,
        policy: Policy,
    ) -> Result<Vec<usize>, Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        validate(points, width, policy)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.choose_inner(points, width, policy)
        }));
        match result {
            Ok(Ok(result)) => {
                self.compositions = self.compositions.saturating_add(1);
                Ok(result)
            }
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                Err(error)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("VM panicked".into()))
            }
        }
    }

    fn choose_inner(
        &mut self,
        points: &[BreakOpportunity],
        width: f32,
        policy: Policy,
    ) -> Result<Vec<usize>, Error> {
        let mut words = Vec::with_capacity(points.len() * 8);
        for point in points {
            words.push(u32::try_from(point.cluster).map_err(|_| Error::Limit)?);
            words.extend(split(point.advance));
            words.extend(split(point.trimmed));
            words.push((point.hyphen as f32).to_bits());
            words.push(u32::from(point.mandatory));
            // Native prepared paragraphs retain their strict no-overflow contract.
            words.push(u32::MAX);
        }
        self.runtime.write("text-points", &words)?;
        self.runtime.write("text-width", &[width.to_bits()])?;
        self.runtime.write(
            "text-policy",
            &[
                policy.hyphen_penalty.to_bits(),
                policy.consecutive_hyphen_penalty.to_bits(),
                policy.last_line_weight.to_bits(),
                policy.edge_limit,
            ],
        )?;
        self.runtime.write("text-output", &vec![0; points.len()])?;
        self.runtime.write("text-result", &[u32::MAX, 0, 0])?;
        self.runtime.run("text-compose-native")?;
        let result = self.runtime.read("text-result")?;
        let [status, count, _edges] = result.as_slice() else {
            return Err(Error::Invalid);
        };
        match status {
            0 => {}
            1 => return Err(Error::Invalid),
            2 => return Err(Error::Unbreakable),
            3 => return Err(Error::Limit),
            _ => return Err(Error::Invalid),
        }
        let output = self.runtime.read("text-output")?;
        let count = usize::try_from(*count).map_err(|_| Error::Invalid)?;
        if count == 0 || count >= points.len() {
            return Err(Error::Invalid);
        }
        let values = output.get(..count).ok_or(Error::Invalid)?;
        values
            .iter()
            .map(|value| usize::try_from(*value).map_err(|_| Error::Invalid))
            .collect::<Result<_, _>>()
    }
}

fn split(value: f64) -> [u32; 2] {
    let high = value as f32;
    [high.to_bits(), ((value - f64::from(high)) as f32).to_bits()]
}

fn validate(points: &[BreakOpportunity], width: f32, policy: Policy) -> Result<(), Error> {
    if !(2..=4096).contains(&points.len()) {
        return Err(Error::Limit);
    }
    if !width.is_finite()
        || width <= 0.
        || width > 1e7
        || policy.edge_limit > 1_000_000
        || [
            policy.hyphen_penalty,
            policy.consecutive_hyphen_penalty,
            policy.last_line_weight,
        ]
        .iter()
        .any(|v| !v.is_finite() || !(0. ..=100.).contains(v))
    {
        return Err(Error::Invalid);
    }
    if points[0].cluster != 0
        || points[0].advance != 0.
        || !points.last().is_some_and(|p| p.mandatory)
    {
        return Err(Error::Invalid);
    }
    for (i, point) in points.iter().enumerate() {
        if [point.advance, point.trimmed, point.hyphen]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=1e14).contains(v))
            || point.trimmed > point.advance
            || (i > 0
                && (point.cluster <= points[i - 1].cluster
                    || point.advance < points[i - 1].advance))
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paragraph_algorithm_runs_in_easl() {
        let mut composer = Composer::new().unwrap();
        let points = [
            (0, 0., 0.),
            (4, 4., 3.),
            (7, 7., 6.),
            (10, 10., 9.),
            (15, 15., 15.),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (cluster, advance, trimmed))| BreakOpportunity {
            cluster,
            advance,
            trimmed,
            hyphen: 0.,
            mandatory: i == 0 || i == 4,
        })
        .collect::<Vec<_>>();
        assert_eq!(
            composer
                .choose_breaks(&points, 6., Policy::default())
                .unwrap(),
            vec![4, 10, 15]
        );
    }

    #[test]
    fn ordinary_easl_program_imports_the_library_and_owns_its_buffers() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/paragraph_policy.easl");
        let documents = easl::parse::load_and_parse_easl_multidocument(&path)
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(documents.sources.len(), 2);
        let mut runtime = runtime::Runtime::from_documents(documents).unwrap();
        runtime.run("main").unwrap();
        let result = runtime.read("result").unwrap();
        assert_eq!(&result[..2], &[0, 3], "{result:?}");
        assert_eq!(&runtime.read("selected").unwrap()[..3], &[4, 10, 15]);
    }
}
