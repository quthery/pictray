#[derive(Debug, Clone, Copy)]
pub struct WindowAnchor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    ShowWindow(Option<WindowAnchor>),
    ToggleWindow(Option<WindowAnchor>),
    CopyLatest,
    RequestClearHistory,
    StorageChanged,
    Quit,
}
