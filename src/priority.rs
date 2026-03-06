use crate::{AppId, backend::BackendName};

/// Determine source priority
pub fn priority(backend_name: BackendName, source_id: &str, _id: &AppId) -> i32 {
    let mut priority = 0;
    // Prefer the flatpak-user backend; among those, prefer flathub
    if backend_name == BackendName::FlatpakUser {
        priority += 2;
        if source_id == "flathub" {
            priority += 1;
        }
    }
    priority
}
