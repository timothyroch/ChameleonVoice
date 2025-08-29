use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};

#[cfg(target_os = "windows")]
use tao::platform::windows::{EventLoopBuilderExtWindows, WindowBuilderExtWindows};

use wry::WebViewBuilder;

/// popup that loads Dashboard UI.
pub fn run_popup(port: u16) {
    let event_loop = EventLoopBuilder::new()
        .with_any_thread(true) // allow launching from non-main thread
        .build();

    let width = 880.0;
    let height = 560.0;

    // Build window
    let mut builder = WindowBuilder::new()
        .with_title("Dashboard")
        .with_inner_size(LogicalSize::new(width, height))
        .with_resizable(false)
        .with_decorations(false) // frameless
        .with_transparent(true)
        .with_always_on_top(true);

    #[cfg(target_os = "windows")]
    {
        builder = builder.with_skip_taskbar(true);
    }

    let window = builder.build(&event_loop).expect("build popup window");

    // Center on primary monitor
    if let Some(monitor) = window.current_monitor() {
        let size = window.outer_size();
        let screen = monitor.size();
        let x = (screen.width as f64 - size.width as f64) / 2.0;
        let y = (screen.height as f64 - size.height as f64) / 2.0;
        let pos = LogicalPosition::new(x / monitor.scale_factor(), y / monitor.scale_factor());
        window.set_outer_position(pos);
    }

    // Load Dashboard via Actix at /dashboard
    let url = format!("http://127.0.0.1:{}/dashboard", port);

    let _webview = WebViewBuilder::new(&window)
        .with_url(&url)
        .build()
        .expect("build webview");

    // event loop
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
