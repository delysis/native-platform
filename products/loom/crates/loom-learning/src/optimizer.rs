use thiserror::Error;

const DEFAULT_BETA_ONE: f32 = 0.9;
const DEFAULT_BETA_TWO: f32 = 0.999;
const DEFAULT_EPSILON: f32 = 1.0e-8;

/// Deterministic, single-thread Adam state for one flat parameter vector.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Adam {
    first_moment: Vec<f32>,
    second_moment: Vec<f32>,
    step: u64,
}

impl Adam {
    pub(crate) fn new(parameter_count: usize) -> Result<Self, OptimizerError> {
        if parameter_count == 0 {
            return Err(OptimizerError::EmptyParameters);
        }
        Ok(Self {
            first_moment: vec![0.0; parameter_count],
            second_moment: vec![0.0; parameter_count],
            step: 0,
        })
    }

    pub(crate) fn update(
        &mut self,
        parameters: &mut [f32],
        gradients: &[f32],
        learning_rate: f32,
    ) -> Result<(), OptimizerError> {
        if parameters.len() != self.first_moment.len() || gradients.len() != parameters.len() {
            return Err(OptimizerError::DimensionMismatch);
        }
        if !learning_rate.is_finite() || learning_rate <= 0.0 {
            return Err(OptimizerError::InvalidLearningRate);
        }
        if gradients.iter().any(|gradient| !gradient.is_finite()) {
            return Err(OptimizerError::NonFiniteGradient);
        }
        if parameters.iter().any(|parameter| !parameter.is_finite()) {
            return Err(OptimizerError::NonFiniteParameter);
        }
        let next_step = self
            .step
            .checked_add(1)
            .ok_or(OptimizerError::StepOverflow)?;
        let step = i32::try_from(next_step).map_err(|_| OptimizerError::StepOverflow)?;
        let first_correction = 1.0 - DEFAULT_BETA_ONE.powi(step);
        let second_correction = 1.0 - DEFAULT_BETA_TWO.powi(step);
        let mut next_parameters = Vec::with_capacity(parameters.len());
        let mut next_first = Vec::with_capacity(parameters.len());
        let mut next_second = Vec::with_capacity(parameters.len());
        for index in 0..parameters.len() {
            let gradient = gradients[index];
            let first_moment = DEFAULT_BETA_ONE.mul_add(
                self.first_moment[index],
                (1.0 - DEFAULT_BETA_ONE) * gradient,
            );
            let second_moment = DEFAULT_BETA_TWO.mul_add(
                self.second_moment[index],
                (1.0 - DEFAULT_BETA_TWO) * gradient * gradient,
            );
            let first = first_moment / first_correction;
            let second = second_moment / second_correction;
            let next =
                parameters[index] - learning_rate * first / (second.sqrt() + DEFAULT_EPSILON);
            if !next.is_finite() {
                return Err(OptimizerError::NonFiniteParameter);
            }
            next_parameters.push(next);
            next_first.push(first_moment);
            next_second.push(second_moment);
        }
        parameters.copy_from_slice(&next_parameters);
        self.first_moment = next_first;
        self.second_moment = next_second;
        self.step = next_step;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum OptimizerError {
    #[error("Adam requires at least one parameter")]
    EmptyParameters,
    #[error("Adam parameter and gradient dimensions differ")]
    DimensionMismatch,
    #[error("Adam learning rate must be finite and positive")]
    InvalidLearningRate,
    #[error("Adam received a non-finite gradient")]
    NonFiniteGradient,
    #[error("Adam produced a non-finite parameter")]
    NonFiniteParameter,
    #[error("Adam step counter overflowed")]
    StepOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_are_bit_reproducible() {
        fn run() -> Vec<f32> {
            let mut values = vec![1.0, -2.0];
            let mut adam = Adam::new(values.len()).expect("Adam");
            for _ in 0..10 {
                adam.update(&mut values, &[0.25, -0.5], 0.01)
                    .expect("update");
            }
            values
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn invalid_update_is_rejected_before_parameter_mutation() {
        let mut values = vec![1.0];
        let before = values.clone();
        let mut adam = Adam::new(1).expect("Adam");
        assert_eq!(
            adam.update(&mut values, &[f32::NAN], 0.01),
            Err(OptimizerError::NonFiniteGradient)
        );
        assert_eq!(values, before);
    }
}
