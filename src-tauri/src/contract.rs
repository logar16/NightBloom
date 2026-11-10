use serde::Serialize;

/// Post-output-tone, pre-encoding RGB pixels in top-down row order.
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
}

#[derive(Clone, Serialize)]
pub struct ExportPlan {
    pub factor: f64,
    pub native_factor: f64,
    pub width: u32,
    pub height: u32,
    pub preview_width: u32,
    pub preview_height: u32,
    pub estimated_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_long_side: Option<u32>,
}
