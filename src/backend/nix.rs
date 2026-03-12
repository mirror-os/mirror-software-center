// SPDX-License-Identifier: GPL-3.0-only
//
// Nix backend for Mirror OS Software Center.
//
// Responsibilities:
//   - Search nixpkgs via `nix search nixpkgs <query> --json`
//   - Install a package by writing a per-app Home Manager module to
//     ~/.config/home-manager/apps/<attr>.nix and running `home-manager switch`
//   - Uninstall by deleting the module file and running `home-manager switch`
//
// The actual `home-manager switch` is delegated to `hm_switch()` which is also
// called by the Flatpak install path.

use std::{
    env,
    error::Error,
    fs,
    path::PathBuf,
    process::Command,
};

/// Return the path to the user's home-manager apps/ directory.
pub fn apps_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home = env::var("HOME")?;
    Ok(PathBuf::from(home).join(".config/home-manager/apps"))
}

/// Write a minimal Home Manager module that enables a package via `programs.<attr>.enable = true`.
/// More complex options are written by the options UI layer before calling `hm_switch`.
pub fn write_nix_module(attr: &str, nix_content: &str) -> Result<(), Box<dyn Error>> {
    let dir = apps_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.nix", attr));
    fs::write(&path, nix_content)?;
    log::info!("wrote nix module: {:?}", path);
    Ok(())
}

/// Delete a per-app Home Manager module. Call `hm_switch` after to apply.
pub fn delete_nix_module(attr: &str) -> Result<(), Box<dyn Error>> {
    let path = apps_dir()?.join(format!("{}.nix", attr));
    if path.exists() {
        fs::remove_file(&path)?;
        log::info!("deleted nix module: {:?}", path);
    }
    Ok(())
}

/// Stage all HM config files and run `home-manager switch`.
/// This is the shared apply step used by both Nix and Flatpak install paths.
pub fn hm_switch() -> Result<(), Box<dyn Error>> {
    let home = env::var("HOME")?;
    let hm_config = PathBuf::from(&home).join(".config/home-manager");
    let username = env::var("USER").unwrap_or_else(|_| "user".to_string());

    // Stage all files so flake evaluation can see new modules
    let stage = Command::new("git")
        .args(["-C", hm_config.to_str().unwrap(), "add", "-A"])
        .output();
    if let Err(e) = stage {
        log::warn!("git add failed (non-fatal): {}", e);
    }

    // Run home-manager switch
    let status = Command::new("home-manager")
        .args([
            "switch",
            "--flake",
            &format!("{}#{}", hm_config.display(), username),
        ])
        .status()?;

    if !status.success() {
        return Err("home-manager switch failed".into());
    }

    log::info!("home-manager switch succeeded");
    Ok(())
}

/// Install a Nix package: write a minimal enable module then run `hm_switch`.
/// For packages with options, the caller should write the full module content
/// via `write_nix_module` and then call `hm_switch` directly.
pub fn install(attr: &str) -> Result<(), Box<dyn Error>> {
    // Strip "nixpkgs." prefix if present (from nix search output)
    let short_attr = attr.trim_start_matches("nixpkgs.");
    let content = format!(
        "# Installed via Mirror OS Software Center\n\
         {{ pkgs, ... }}:\n\
         {{\n\
           home.packages = [ pkgs.{short_attr} ];\n\
         }}\n"
    );
    write_nix_module(short_attr, &content)?;
    hm_switch()
}

/// Uninstall a Nix package by removing its module file.
pub fn uninstall(attr: &str) -> Result<(), Box<dyn Error>> {
    let short_attr = attr.trim_start_matches("nixpkgs.");
    delete_nix_module(short_attr)?;
    hm_switch()
}

/// Check whether a per-app module exists (i.e. the package is installed).
pub fn is_installed(attr: &str) -> bool {
    let short_attr = attr.trim_start_matches("nixpkgs.");
    apps_dir()
        .map(|d| d.join(format!("{}.nix", short_attr)).exists())
        .unwrap_or(false)
}
