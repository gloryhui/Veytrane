//! Keyboard and command actions for the Agent desktop and text fields.
use gpui::actions;
pub(crate) use veytrane_desktop_ui::text_actions::*;

actions!(
    desktop_shell,
    [
        ChooseFiles,
        ChooseFolder,
        Quit,
        ConnectPeer,
        StartSpeed,
        ToggleSettings,
        NextField,
        PreviousField,
    ]
);
