use anyhow::Result;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, px, size};
use std::time::Duration;
use uuid::Uuid;

/// The single built-in display, sized from the native window.
#[derive(Debug, Clone)]
pub(crate) struct AndroidDisplay {
    pub size: gpui::Size<Pixels>,
    pub refresh_hz: f64,
}

impl AndroidDisplay {
    pub fn new(width: f32, height: f32, refresh_hz: f64) -> Self {
        Self { size: size(px(width), px(height)), refresh_hz }
    }
}

impl PlatformDisplay for AndroidDisplay {
    fn id(&self) -> DisplayId {
        DisplayId::new(1)
    }

    fn uuid(&self) -> Result<Uuid> {
        Ok(Uuid::new_v5(&Uuid::NAMESPACE_OID, b"android-builtin-display"))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds { origin: Default::default(), size: self.size }
    }

    fn refresh_interval(&self) -> Option<Duration> {
        gpui::refresh_interval_from_hz(self.refresh_hz)
    }
}
