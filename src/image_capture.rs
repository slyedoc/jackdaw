//! A read-back of one offscreen [`RenderTarget::Image`](bevy::camera::RenderTarget), shaped
//! like bevy's `Screenshot::image` so its callers survive the move off `bevy_render`.
//!
//! TODO(aurora): nothing fulfils an [`ImageCapture`] yet. Aurora reads back only the
//! swapchain (`ScreenshotRequests`); an image-target readback that triggers
//! [`ImageCaptured`] brings back thumbnails, camera captures, viewport screenshots and the
//! PIE frame view at once.

use bevy::prelude::*;

/// Ask for the next frame drawn into this image, delivered as [`ImageCaptured`] on this
/// entity.
#[derive(Component, Clone, Debug)]
pub struct ImageCapture(pub Handle<Image>);

impl ImageCapture {
    pub fn image(target: impl Into<Handle<Image>>) -> Self {
        Self(target.into())
    }
}

/// The frame an [`ImageCapture`] asked for.
#[derive(EntityEvent, Clone, Debug)]
pub struct ImageCaptured {
    pub entity: Entity,
    pub image: Image,
}
