//! Keyboard and command actions for the Agent desktop and text fields.
use gpui::actions;

actions!(
    desktop_shell,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
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
