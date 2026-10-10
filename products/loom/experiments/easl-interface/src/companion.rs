//! Optional OS webview beside the native EASL surface. The writing renderer
//! does not depend on this view, its HTML, or its text metrics.
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    window::Window,
};

pub struct Companion {
    view: wry::WebView,
    bounds: Option<[f32; 4]>,
}
impl Companion {
    pub fn new(window: &Window) -> Result<Self, String> {
        let view = wry::WebViewBuilder::new()
            .with_visible(false)
            .with_accept_first_mouse(true)
            .with_navigation_handler(|url| url == "about:blank")
            .with_html(r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><style>html{background:#f2f0eb;color:#30382f;font:14px/1.6 system-ui}body{margin:0;padding:12px}h2{font:22px Georgia;margin:0 0 12px}input{box-sizing:border-box;width:100%;padding:8px;border:1px solid #aab3a4;background:#faf8f2;font:inherit}</style><h2>A companion view</h2><p>This small reference pane uses the platform webview. The manuscript and notes beside it use native EASL layout and native glyph rendering.</p><label for="probe">Webview focus check</label><input id="probe" placeholder="Type here, then return to the page"></html>"#)
            .build_as_child(window).map_err(|e| e.to_string())?;
        Ok(Self { view, bounds: None })
    }
    pub fn update(&mut self, bounds: Option<[f32; 4]>) -> Result<(), String> {
        // EASL geometry consists of finite validated logical pixels. Exact
        // equality is intentional: avoid redundant OS view transactions.
        if self.bounds.as_ref().map(|r| r.map(f32::to_bits))
            == bounds.as_ref().map(|r| r.map(f32::to_bits))
        {
            return Ok(());
        }
        if let Some([x, y, width, height]) = bounds {
            self.view
                .set_bounds(wry::Rect {
                    position: LogicalPosition::new(x, y).into(),
                    size: LogicalSize::new(width, height).into(),
                })
                .map_err(|e| e.to_string())?;
        }
        self.view
            .set_visible(bounds.is_some())
            .map_err(|e| e.to_string())?;
        self.bounds = bounds;
        Ok(())
    }
}
