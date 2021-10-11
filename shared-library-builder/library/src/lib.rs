mod cairo_library;
mod pixman_library;

use crate::cairo_library::CairoLibrary;
use shared_library_builder::{GitLocation, LibraryLocation};

pub fn libcairo(binary_version: Option<impl Into<String>>) -> CairoLibrary {
    CairoLibrary::default().with_release_location(binary_version.map(|version| {
        LibraryLocation::Git(GitLocation::github("feenkcom", "libcairo").tag(version))
    }))
}
