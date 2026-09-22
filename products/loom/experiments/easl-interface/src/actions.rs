//! Validate the complete synchronous view-intent batch before invoking a host.
//! These are presentation commands, not transferable document capabilities.
//! Asynchronous service replies still require their existing owner/revision checks.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Edit(u32),
    Click(u32),
    Scroll(u32),
    Save,
    OpenProject,
    NewDocument,
    Close,
    SelectDocument(u32),
    Reload,
    Drag(u32),
    Source(u32),
    Format(u32),
    Appearance(u32),
    FormatMenu(bool),
    DragWindow,
    Auxiliary(u32),
    TogglePane(u32),
    PaneMenu(Option<u32>),
    SelectPane(u32),
    OpenPane(u32),
    PaneKey(u32),
    Resize(u32),
    ResizeKey(u32),
    InputPointer(bool),
    AddMenu(bool),
    AddNew,
}

impl Action {
    pub fn from_wire(kind: u32, argument: u32) -> Result<Self, String> {
        Ok(match (kind, argument) {
            (1, id @ 0..=9) => Self::Edit(id),
            (2, id @ 1..=9) => Self::Click(id),
            (3, id @ 1..=9) => Self::Scroll(id),
            (4, 0) => Self::Save,
            (5, 0) => Self::OpenProject,
            (6, 0) => Self::NewDocument,
            (7, 0) => Self::Close,
            (8, id @ 0..=4096) => Self::SelectDocument(id),
            (9, 0) => Self::Reload,
            (10, id @ 1..=9) => Self::Drag(id),
            (11, id @ 0..=9) => Self::Source(id),
            (12, id @ 0..=10) => Self::Format(id),
            (13, mode @ 0..=2) => Self::Appearance(mode),
            (14, open @ 0..=1) => Self::FormatMenu(open != 0),
            (15, 0) => Self::DragWindow,
            (16, id @ 0..=4096) => Self::Auxiliary(id),
            (17, position @ 0..=2) => Self::TogglePane(position),
            (18, position @ 0..=2) => Self::PaneMenu(Some(position)),
            (18, 3) => Self::PaneMenu(None),
            (19, choice @ 0..=7) => Self::SelectPane(choice),
            (20, position @ 0..=2) => Self::OpenPane(position),
            (21, key @ 0..=29) => Self::PaneKey(key),
            (22, divider @ 1..=3) => Self::Resize(divider),
            (23, key @ 0..=29) => Self::ResizeKey(key),
            (24, drag @ 0..=1) => Self::InputPointer(drag != 0),
            (25, open @ 0..=1) => Self::AddMenu(open != 0),
            (26, 0) => Self::AddNew,
            _ => return Err(format!("Invalid native view action {kind}:{argument}")),
        })
    }

    pub fn wire(self) -> (u32, u32) {
        match self {
            Self::Edit(id) => (1, id),
            Self::Click(id) => (2, id),
            Self::Scroll(id) => (3, id),
            Self::Save => (4, 0),
            Self::OpenProject => (5, 0),
            Self::NewDocument => (6, 0),
            Self::Close => (7, 0),
            Self::SelectDocument(id) => (8, id),
            Self::Reload => (9, 0),
            Self::Drag(id) => (10, id),
            Self::Source(id) => (11, id),
            Self::Format(id) => (12, id),
            Self::Appearance(mode) => (13, mode),
            Self::FormatMenu(open) => (14, u32::from(open)),
            Self::DragWindow => (15, 0),
            Self::Auxiliary(id) => (16, id),
            Self::TogglePane(position) => (17, position),
            Self::PaneMenu(position) => (18, position.unwrap_or(3)),
            Self::SelectPane(choice) => (19, choice),
            Self::OpenPane(position) => (20, position),
            Self::PaneKey(key) => (21, key),
            Self::Resize(id) => (22, id),
            Self::ResizeKey(key) => (23, key),
            Self::InputPointer(drag) => (24, u32::from(drag)),
            Self::AddMenu(open) => (25, u32::from(open)),
            Self::AddNew => (26, 0),
        }
    }

    fn dismissal(self) -> bool {
        matches!(
            self,
            Self::FormatMenu(false) | Self::PaneMenu(None) | Self::AddMenu(false)
        )
    }

    fn permits_event(self, event: u32) -> bool {
        match event {
            1 => matches!(
                self,
                Self::Click(_)
                    | Self::Save
                    | Self::OpenProject
                    | Self::NewDocument
                    | Self::Close
                    | Self::SelectDocument(_)
                    | Self::Source(_)
                    | Self::Format(_)
                    | Self::Appearance(_)
                    | Self::FormatMenu(_)
                    | Self::DragWindow
                    | Self::Auxiliary(_)
                    | Self::TogglePane(_)
                    | Self::PaneMenu(_)
                    | Self::SelectPane(_)
                    | Self::OpenPane(_)
                    | Self::Resize(_)
                    | Self::InputPointer(false)
                    | Self::AddMenu(_)
                    | Self::AddNew
            ),
            2 => matches!(
                self,
                Self::Edit(_)
                    | Self::Save
                    | Self::OpenProject
                    | Self::NewDocument
                    | Self::Close
                    | Self::Reload
                    | Self::Source(_)
                    | Self::Format(_)
                    | Self::Appearance(_)
                    | Self::FormatMenu(_)
                    | Self::PaneKey(_)
                    | Self::ResizeKey(_)
                    | Self::AddMenu(_)
                    | Self::AddNew
            ),
            3 => matches!(self, Self::Scroll(_)),
            4 => self == Self::Close,
            5 => matches!(self, Self::Drag(_) | Self::InputPointer(true)),
            _ => false,
        }
    }
}

pub fn admit(event: u32, wire: &[(u32, u32)]) -> Result<Vec<Action>, String> {
    if event > 5 || wire.len() > 4 {
        return Err("Invalid native event or action batch bound".into());
    }
    let mut actions = Vec::with_capacity(wire.len());
    let mut primary = false;
    for &(kind, argument) in wire {
        let action = Action::from_wire(kind, argument)?;
        if !action.permits_event(event) || actions.contains(&action) {
            return Err("Duplicate or out-of-phase native view action".into());
        }
        if !action.dismissal() && action != Action::Edit(0) {
            if primary {
                return Err("Multiple primary actions in one native event".into());
            }
            primary = true;
        }
        actions.push(action);
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_frames_never_dispatch_actions() {
        assert!(admit(0, &[]).unwrap().is_empty());
        for kind in 1..=26 {
            assert!(admit(0, &[(kind, 0)]).is_err());
        }
    }

    #[test]
    fn complete_batch_is_rejected_before_any_command_can_be_observed() {
        for invalid in [
            vec![(6, 0), (999, 0)],
            vec![(6, 0), (6, 0)],
            vec![(6, 0), (7, 0)],
        ] {
            assert!(admit(1, &invalid).is_err());
        }
        assert!(admit(1, &[(14, 0), (17, 1)]).is_ok());
        assert_eq!(admit(1, &[(18, 3)]).unwrap(), [Action::PaneMenu(None)]);
    }

    #[test]
    fn every_accepted_wire_value_round_trips_and_bad_arguments_reject() {
        for kind in 0..=27 {
            for argument in 0..=4097 {
                if let Ok(action) = Action::from_wire(kind, argument) {
                    assert_eq!(action.wire(), (kind, argument));
                }
            }
        }
        for (kind, argument) in [(4, 1), (14, 2), (17, 3), (18, 4), (22, 0), (24, 2), (26, 1)] {
            assert!(Action::from_wire(kind, argument).is_err());
        }
    }

    #[test]
    fn event_phases_cannot_smuggle_edit_click_or_close_commands() {
        for (event, accepted) in [
            (1, (2, 1)),
            (2, (1, 1)),
            (3, (3, 1)),
            (4, (7, 0)),
            (5, (10, 1)),
        ] {
            assert!(admit(event, &[accepted]).is_ok());
        }
        for (event, forbidden) in [
            (1, (1, 1)),
            (2, (15, 0)),
            (3, (6, 0)),
            (4, (4, 0)),
            (5, (2, 1)),
        ] {
            assert!(admit(event, &[forbidden]).is_err());
        }
        assert!(admit(6, &[]).is_err());
        assert!(admit(1, &[(14, 0); 5]).is_err());
    }
}
