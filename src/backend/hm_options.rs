// SPDX-License-Identifier: GPL-3.0-only
//
// Home Manager option introspection for Mirror OS Software Center.
//
// Uses `nix eval --json` against the user's home-manager flake to retrieve
// the full option schema for a given attribute path (e.g. "programs.neovim").
// Results are cached in memory to avoid repeated ~2–5s nix eval calls.

use std::{
    collections::HashMap,
    env,
    error::Error,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

/// A single Home Manager option, as returned by the nix eval option schema.
#[derive(Clone, Debug)]
pub struct HmOption {
    /// Dot-separated path, e.g. "programs.neovim.enable"
    pub path: String,
    /// Human-readable description
    pub description: String,
    /// The option type (see `OptionType`)
    pub option_type: OptionType,
    /// Default value as a Nix expression string, if any
    pub default: Option<String>,
    /// Current value as a Nix expression string, if declared in the user's config
    pub current_value: Option<String>,
}

/// The type of a Home Manager option, used to drive the appropriate widget.
#[derive(Clone, Debug)]
pub enum OptionType {
    /// Renders as a toggle/switch
    Bool,
    /// Renders as a text input field
    String,
    /// Renders as a text input field (file path with completion, eventually)
    Path,
    /// Renders as a dropdown with fixed choices
    Enum(Vec<String>),
    /// Renders as a numeric spinner
    Int,
    /// Renders as a numeric spinner with decimals
    Float,
    /// Renders as a tag input / multi-value list
    ListOf(Box<OptionType>),
    /// Renders as a nested expandable section
    Attrs,
    /// Fallback for types we don't yet represent
    Unknown(String),
}

/// A tree of options for a given attribute path.
#[derive(Clone, Debug, Default)]
pub struct OptionTree {
    pub options: Vec<HmOption>,
}

/// In-memory cache: attr path → parsed OptionTree
static CACHE: std::sync::LazyLock<Mutex<HashMap<String, Arc<OptionTree>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Retrieve the Home Manager option schema for `attr_path` (e.g. "programs.neovim").
///
/// Shells out to:
///   nix eval --json \
///     ~/.config/home-manager#homeConfigurations.$USER.options.<attr_path>
///
/// Results are cached in memory for the lifetime of the process.
pub fn get_options(attr_path: &str) -> Result<Arc<OptionTree>, Box<dyn Error>> {
    // Check cache first
    {
        let cache = CACHE.lock().unwrap();
        if let Some(tree) = cache.get(attr_path) {
            return Ok(Arc::clone(tree));
        }
    }

    let home = env::var("HOME")?;
    let username = env::var("USER").unwrap_or_else(|_| "user".to_string());
    let hm_config = PathBuf::from(&home).join(".config/home-manager");

    let flake_ref = format!(
        "{}#homeConfigurations.{}.options.{}",
        hm_config.display(),
        username,
        attr_path
    );

    log::info!("nix eval options for: {}", attr_path);

    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--extra-experimental-features",
            "nix-command flakes",
            &flake_ref,
        ])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "nix eval failed for {}: {}",
            attr_path,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let tree = Arc::new(parse_option_tree(attr_path, &json));

    // Store in cache
    {
        let mut cache = CACHE.lock().unwrap();
        cache.insert(attr_path.to_string(), Arc::clone(&tree));
    }

    Ok(tree)
}

/// Parse a JSON option schema object returned by nix eval into an `OptionTree`.
fn parse_option_tree(prefix: &str, json: &serde_json::Value) -> OptionTree {
    let mut options = Vec::new();
    collect_options(prefix, json, &mut options);
    OptionTree { options }
}

/// Recursively walk the JSON option schema and collect leaf options.
fn collect_options(path: &str, json: &serde_json::Value, out: &mut Vec<HmOption>) {
    let obj = match json.as_object() {
        Some(o) => o,
        None => return,
    };

    // If this node has a "_type" key equal to "option", it's a leaf option
    if obj.get("_type").and_then(|v| v.as_str()) == Some("option") {
        let description = obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let type_str = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let default = obj
            .get("default")
            .map(|v| v.to_string());

        let option_type = parse_option_type(&type_str, obj);

        out.push(HmOption {
            path: path.to_string(),
            description,
            option_type,
            default,
            current_value: None,
        });
        return;
    }

    // Otherwise recurse into sub-options
    for (key, val) in obj {
        if key.starts_with('_') {
            continue;
        }
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", path, key)
        };
        collect_options(&child_path, val, out);
    }
}

/// Map a Home Manager type string to our `OptionType` enum.
fn parse_option_type(type_str: &str, obj: &serde_json::Map<String, serde_json::Value>) -> OptionType {
    match type_str {
        "boolean" => OptionType::Bool,
        "string" | "non-empty string" | "strings concatenated with \"\\n\"" => OptionType::String,
        "path" => OptionType::Path,
        "integer" | "positive integer" | "unsigned integer, meaning >=0" => OptionType::Int,
        "float" => OptionType::Float,
        t if t.starts_with("one of ") => {
            // "one of \"a\", \"b\", \"c\""
            let variants: Vec<String> = t
                .trim_start_matches("one of ")
                .split(", ")
                .map(|s| s.trim_matches('"').to_string())
                .collect();
            OptionType::Enum(variants)
        }
        t if t.starts_with("list of ") => {
            let inner_type = t.trim_start_matches("list of ");
            OptionType::ListOf(Box::new(parse_option_type(inner_type, obj)))
        }
        t if t.contains("attribute set") => OptionType::Attrs,
        t => OptionType::Unknown(t.to_string()),
    }
}
