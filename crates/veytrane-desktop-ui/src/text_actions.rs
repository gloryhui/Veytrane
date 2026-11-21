//! Text-field actions shared by the desktop shell and optional file UI.

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
    ]
);
