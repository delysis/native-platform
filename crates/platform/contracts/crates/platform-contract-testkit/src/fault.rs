use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    BeforeAdmission,
    AfterAdmission,
    BeforeTerminal,
    AfterTerminal,
    BeforeRelease,
    CorruptStagedBeforeVerification,
    BeforeFileSync,
    BeforeVisibility,
    AfterNoClobberVisibility,
    BeforeVisibleFileSync,
    BeforeParentSync,
    AfterVisibilityBeforeParentSync,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InjectedFault(pub FaultPoint);

#[derive(Clone, Debug, Default)]
pub struct FaultScript {
    points: VecDeque<FaultPoint>,
}

impl FaultScript {
    #[must_use]
    pub fn new(points: impl IntoIterator<Item = FaultPoint>) -> Self {
        Self {
            points: points.into_iter().collect(),
        }
    }

    pub fn hit(&mut self, point: FaultPoint) -> Result<(), InjectedFault> {
        if self.points.front() == Some(&point) {
            self.points.pop_front();
            return Err(InjectedFault(point));
        }
        Ok(())
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.points.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_fire_once_in_declared_order() {
        let mut script = FaultScript::new([FaultPoint::AfterAdmission, FaultPoint::BeforeTerminal]);
        assert!(script.hit(FaultPoint::BeforeAdmission).is_ok());
        assert_eq!(
            script.hit(FaultPoint::AfterAdmission),
            Err(InjectedFault(FaultPoint::AfterAdmission))
        );
        assert!(script.hit(FaultPoint::AfterAdmission).is_ok());
        assert_eq!(script.remaining(), 1);
    }
}
