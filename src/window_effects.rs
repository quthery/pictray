#[cfg(target_os = "windows")]
mod windows {
    use std::{ffi::c_void, mem, sync::OnceLock};

    use slint::winit_030::winit::platform::windows::{CornerPreference, WindowExtWindows};
    use slint::winit_030::winit::{
        self,
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
    };
    use windows_sys::Win32::{
        Foundation::{BOOL, HWND},
        Graphics::Dwm::{
            DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmExtendFrameIntoClientArea,
            DwmSetWindowAttribute,
        },
        System::LibraryLoader::{GetProcAddress, LoadLibraryA},
        UI::Controls::MARGINS,
    };

    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_ENABLE_BLURBEHIND: u32 = 3;

    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttribData {
        attrib: u32,
        pv_data: *mut c_void,
        cb_data: usize,
    }

    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> BOOL;

    static SET_WINDOW_COMPOSITION_ATTRIBUTE: OnceLock<Option<SetWindowCompositionAttribute>> =
        OnceLock::new();

    pub fn apply_to_window(window: &winit::window::Window) {
        let Some(hwnd) = hwnd_from_window(window) else {
            return;
        };

        window.set_corner_preference(CornerPreference::Round);

        unsafe {
            // Let DWM render the backdrop across the full client area of the frameless Slint window.
            let margins = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

            // Prefer the official Windows 11 acrylic backdrop when available.
            let backdrop = DWMSBT_TRANSIENTWINDOW;
            let hr = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE as u32,
                &backdrop as *const _ as *const c_void,
                mem::size_of_val(&backdrop) as u32,
            );

            if hr < 0 {
                apply_composition_blur(hwnd);
            }
        }
    }

    fn hwnd_from_window(window: &winit::window::Window) -> Option<HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        }
    }

    unsafe fn apply_composition_blur(hwnd: HWND) {
        let Some(set_window_composition_attribute) = composition_api() else {
            return;
        };

        let mut policy = AccentPolicy {
            accent_state: ACCENT_ENABLE_BLURBEHIND,
            accent_flags: 0,
            gradient_color: 0,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttribData {
            attrib: WCA_ACCENT_POLICY,
            pv_data: &mut policy as *mut _ as *mut c_void,
            cb_data: mem::size_of::<AccentPolicy>(),
        };

        unsafe {
            let _ = set_window_composition_attribute(hwnd, &mut data);
        }
    }

    fn composition_api() -> Option<SetWindowCompositionAttribute> {
        *SET_WINDOW_COMPOSITION_ATTRIBUTE.get_or_init(|| unsafe {
            let module = LoadLibraryA(b"user32.dll\0".as_ptr());
            if module.is_null() {
                return None;
            }

            let proc = GetProcAddress(module, b"SetWindowCompositionAttribute\0".as_ptr())?;
            Some(mem::transmute::<
                unsafe extern "system" fn() -> isize,
                SetWindowCompositionAttribute,
            >(proc))
        })
    }
}

#[cfg(target_os = "windows")]
pub fn apply_to_window(window: &slint::winit_030::winit::window::Window) {
    windows::apply_to_window(window);
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSColor, NSView, NSWindow};
    use objc2_quartz_core::CALayer;
    use slint::winit_030::winit::{
        self,
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
    };

    const WINDOW_CORNER_RADIUS: f64 = 26.0;

    pub fn apply_to_window(window: &winit::window::Window) {
        let Some(_mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(slint_view) = ns_view_from_window(window) else {
            return;
        };
        let Some(ns_window) = slint_view.window() else {
            return;
        };

        let content_frame = ns_window
            .contentView()
            .map(|view| view.frame())
            .unwrap_or_else(|| slint_view.frame());

        configure_window(&ns_window);
        configure_slint_view(&slint_view, content_frame);
    }

    fn ns_view_from_window(window: &winit::window::Window) -> Option<Retained<NSView>> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(handle) => unsafe {
                Retained::retain(handle.ns_view.as_ptr().cast())
            },
            _ => None,
        }
    }

    fn configure_window(window: &NSWindow) {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
    }

    fn configure_slint_view(slint_view: &NSView, frame: objc2_foundation::NSRect) {
        slint_view.setFrame(frame);
        slint_view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        clip_view_corners(slint_view);
    }

    fn clip_view_corners(view: &NSView) {
        view.setWantsLayer(true);

        let Some(layer): Option<Retained<CALayer>> = view.layer() else {
            return;
        };

        layer.setCornerRadius(WINDOW_CORNER_RADIUS);
        layer.setMasksToBounds(true);
    }
}

#[cfg(target_os = "macos")]
pub fn apply_to_window(window: &slint::winit_030::winit::window::Window) {
    macos::apply_to_window(window);
}
