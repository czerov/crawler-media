//! Home/playback views: up-next, favorites wall, favorites gallery.

pub(crate) mod favorites;
pub(crate) mod gallery;
pub(crate) mod up_next;

pub(crate) use favorites::favorites;
pub(crate) use gallery::favorites_gallery;
pub(crate) use up_next::up_next;
