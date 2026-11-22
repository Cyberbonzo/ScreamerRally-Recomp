//! What the keys and the panel's buttons do.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Lock the editing camera cell, or follow the game's camera again.
    ToggleLock,
    /// Keep the car in view.
    ToggleFollow,
    Fit,
    ZoomIn,
    ZoomOut,
    /// The road extension (whole track): further along the road, less far.
    Extend,
    Less,
    /// The road extension's width beside the road.
    WidthDown,
    WidthUp,
    /// The edit area (camera cells a click changes) along the road: smaller, larger.
    AreaDown,
    AreaUp,
    /// Drop the edits of the area's camera cells.
    ResetArea,
    Undo,
    Redo,
    Save,
    ToggleBlink,
    ToggleGrid,
    ToggleTextures,
}

impl Action {
    pub const ALL: [Action; 18] = [
        Action::ToggleLock,
        Action::ToggleFollow,
        Action::Fit,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::Extend,
        Action::Less,
        Action::WidthDown,
        Action::WidthUp,
        Action::AreaDown,
        Action::AreaUp,
        Action::ResetArea,
        Action::Undo,
        Action::Redo,
        Action::Save,
        Action::ToggleBlink,
        Action::ToggleGrid,
        Action::ToggleTextures,
    ];

    /// A short name (the test script's `button NAME`, the log).
    pub fn id(self) -> &'static str {
        match self {
            Action::ToggleLock => "lock",
            Action::ToggleFollow => "follow",
            Action::Fit => "fit",
            Action::ZoomIn => "zoom+",
            Action::ZoomOut => "zoom-",
            Action::Extend => "extend",
            Action::Less => "less",
            Action::WidthDown => "width-",
            Action::WidthUp => "width+",
            Action::AreaDown => "area-",
            Action::AreaUp => "area+",
            Action::ResetArea => "reset",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::Save => "save",
            Action::ToggleBlink => "blink",
            Action::ToggleGrid => "grid",
            Action::ToggleTextures => "textures",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id().eq_ignore_ascii_case(id))
    }

    /// What it does, for the status line while the mouse is on its button.
    pub fn help(self) -> &'static str {
        match self {
            Action::ToggleLock => "Lock the camera cell, or follow the game's camera again (L; Ctrl+click picks a cell)",
            Action::ToggleFollow => "Keep the car in view (F)",
            Action::Fit => "Show the whole track (Home)",
            Action::ZoomIn => "Zoom in (+ or the wheel)",
            Action::ZoomOut => "Zoom out (- or the wheel)",
            Action::Extend => "Draw further along the road, everywhere on the track, as you drive; again: further still (E)",
            Action::Less => "Draw less far along the road; down to the designers' lists (Shift+E)",
            Action::WidthDown => "The road extension takes less beside the road (key <)",
            Action::WidthUp => "The road extension takes more beside the road (key >)",
            Action::AreaDown => "Your clicks change less of the road around you (key [; 0: this camera cell only)",
            Action::AreaUp => "Your clicks change more of the road around you, both ways (key ])",
            Action::ResetArea => "Drop your clicks in the area around you: back to the designers' lists and the road (Backspace)",
            Action::Undo => "Undo (Ctrl+Z)",
            Action::Redo => "Redo (Ctrl+Y)",
            Action::Save => "Save now (Ctrl+S; edits are also saved a second after each change)",
            Action::ToggleBlink => "The sector under the mouse blinks in the game (B)",
            Action::ToggleGrid => "Sector grid lines (G)",
            Action::ToggleTextures => "The game's textures, or flat colours (T)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_id(a.id()), Some(a));
            assert_eq!(Action::ALL.iter().filter(|b| b.id() == a.id()).count(), 1);
        }
        assert_eq!(Action::from_id("EXTEND"), Some(Action::Extend));
        assert_eq!(Action::from_id("nothing"), None);
    }
}
