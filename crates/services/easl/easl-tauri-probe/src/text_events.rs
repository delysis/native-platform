//! Tao can emit a commit-text echo immediately before an ordinary key event.
//! The echo never authorizes an edit. Only matching current key text does; an
//! unmatched commit or any intervening input/focus/batch boundary fails closed.
#[derive(Debug, Default)]
pub(crate) struct TextEvents {
    pending: Option<String>,
    faulted: bool,
}
impl TextEvents {
    pub fn receive(&mut self, text: &str, focused: bool) -> Result<(), &'static str> {
        let repeated = self.pending.take().is_some();
        if !focused
            || self.faulted
            || repeated
            || text.is_empty()
            || text.len() > easl_native_text::MAX_TEXT_BYTES
            || text.chars().any(char::is_control)
        {
            self.faulted = true;
            return Err("composition-binding-unavailable");
        }
        self.pending = Some(text.to_owned());
        Ok(())
    }
    /// `text` must come from an admitted ordinary key edit, not the echo itself.
    pub fn key(&mut self, text: Option<&str>) -> Result<(), &'static str> {
        if self.faulted {
            return Err("composition-binding-unavailable");
        }
        if self
            .pending
            .take()
            .is_some_and(|pending| Some(pending.as_str()) != text)
        {
            self.faulted = true;
            return Err("composition-binding-unavailable");
        }
        Ok(())
    }
    /// Commits cannot cross focus, pointer, resize, close, or event-batch boundaries.
    pub fn barrier(&mut self) -> Result<(), &'static str> {
        if self.pending.take().is_some() {
            self.faulted = true;
            Err("composition-binding-unavailable")
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn macos_echo_pairs_with_current_key_text_without_becoming_an_edit() {
        let mut events = TextEvents::default();
        events.receive("é", true).unwrap();
        events.key(Some("é")).unwrap();
        events.barrier().unwrap();
        // A repeat need not emit another echo.
        events.key(Some("é")).unwrap();
    }
    #[test]
    fn unbound_or_mismatched_commit_is_rejected_and_cannot_replay() {
        for key in [None, Some("different")] {
            let mut events = TextEvents::default();
            events.receive("private composition", true).unwrap();
            assert_eq!(events.key(key), Err("composition-binding-unavailable"));
            assert!(events.key(Some("later")).is_err());
            events.barrier().unwrap();
        }
    }
    #[test]
    fn focus_or_event_batch_boundary_revokes_a_pending_commit() {
        let mut blurred = TextEvents::default();
        assert!(blurred.receive("foreign commit", false).is_err());
        assert!(blurred.key(Some("ordinary")).is_err());
        let mut events = TextEvents::default();
        events.receive("old field", true).unwrap();
        assert!(events.barrier().is_err());
        assert!(events.key(Some("new field")).is_err());
        events.barrier().unwrap();
    }
    #[test]
    fn a_second_unpaired_text_event_cannot_replace_the_first_silently() {
        let mut events = TextEvents::default();
        events.receive("first", true).unwrap();
        assert!(events.receive("second", true).is_err());
        events.barrier().unwrap();
        assert!(events.key(Some("ordinary")).is_err());
    }
    #[test]
    fn empty_control_and_over_budget_echoes_are_not_retained() {
        for text in [
            String::new(),
            "\n".into(),
            "x".repeat(easl_native_text::MAX_TEXT_BYTES + 1),
        ] {
            let mut events = TextEvents::default();
            assert!(events.receive(&text, true).is_err());
            events.barrier().unwrap();
        }
    }
}
