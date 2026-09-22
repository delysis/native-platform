//! Actual native-resource ownership and redraw coalescing, independent of an OS.
//! Closing takes the resource, not just a boolean flag. Late events cannot revive it.
#[derive(Debug)]
pub struct SurfaceSlot<Id, T> {
    id: Id,
    value: Option<T>,
    redraw_pending: bool,
}
impl<Id: Eq, T> SurfaceSlot<Id, T> {
    pub fn new(id: Id, value: T) -> Self {
        Self {
            id,
            value: Some(value),
            redraw_pending: false,
        }
    }
    pub fn owner(&self) -> &Id {
        &self.id
    }
    pub fn get_mut(&mut self, id: &Id) -> Option<&mut T> {
        if id == &self.id {
            self.value.as_mut()
        } else {
            None
        }
    }
    /// True means the caller must issue the one missing native redraw request.
    pub fn invalidate(&mut self, id: &Id) -> bool {
        if id != &self.id || self.value.is_none() || self.redraw_pending {
            return false;
        }
        self.redraw_pending = true;
        true
    }
    /// OS exposure may require repaint even without our own pending request.
    pub fn redraw(&mut self, id: &Id) -> Option<&mut T> {
        if id != &self.id {
            return None;
        }
        self.redraw_pending = false;
        self.value.as_mut()
    }
    /// Release buffers/context/window references before the runtime closes its window.
    pub fn close(&mut self, id: &Id) -> bool {
        if id != &self.id {
            return false;
        }
        self.redraw_pending = false;
        self.value.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Resource(Rc<Cell<u32>>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn closing_releases_real_resources_once_and_rejects_late_events() {
        let drops = Rc::new(Cell::new(0));
        let mut slot = SurfaceSlot::new(7, Resource(drops.clone()));
        assert!(!slot.close(&8));
        assert!(slot.get_mut(&8).is_none());
        assert_eq!(drops.get(), 0);
        assert!(slot.close(&7));
        assert_eq!(drops.get(), 1);
        assert!(!slot.close(&7));
        assert!(slot.get_mut(&7).is_none());
        assert!(slot.redraw(&7).is_none());
        assert!(!slot.invalidate(&7));
        drop(slot);
        assert_eq!(drops.get(), 1);
    }
    #[test]
    fn repeated_invalidations_request_one_redraw_but_exposure_still_paints() {
        let mut slot = SurfaceSlot::new(7, 0);
        assert!(!slot.invalidate(&8));
        assert!(slot.invalidate(&7));
        for _ in 0..1000 {
            assert!(!slot.invalidate(&7));
        }
        assert!(slot.redraw(&8).is_none());
        assert!(!slot.invalidate(&7));
        *slot.redraw(&7).unwrap() += 1;
        assert!(slot.invalidate(&7));
        *slot.redraw(&7).unwrap() += 1;
        *slot.redraw(&7).unwrap() += 1;
        assert_eq!(*slot.get_mut(&7).unwrap(), 3);
    }
}
