//! Hyo Sheet — a spreadsheet workspace built with iced and wgpu.

mod app;
mod demo;
mod grid;

fn main() -> iced::Result {
    // iced's wgpu backend reads these through wgpu's environment support.
    // Setting them before the runtime starts pins the app to Vulkan while
    // still allowing an explicit override from the shell.
    if std::env::var_os("WGPU_BACKEND").is_none() {
        // SAFETY: no other threads exist before iced starts.
        unsafe {
            std::env::set_var("WGPU_BACKEND", "vulkan");
        }
    }
    if std::env::var_os("WGPU_POWER_PREF").is_none() {
        // SAFETY: no other threads exist before iced starts.
        unsafe {
            std::env::set_var("WGPU_POWER_PREF", "high");
        }
    }

    iced::application(app::HyoApp::new, app::update, app::view)
        .title(app::title)
        .subscription(app::subscription)
        .theme(|_: &app::HyoApp| iced::Theme::Dark)
        .window_size([1280.0, 820.0])
        .antialiasing(true)
        .run()
}
