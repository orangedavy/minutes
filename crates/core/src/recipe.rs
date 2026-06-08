//! Recipe system — meeting-type-aware prompts that fully control extraction.
//!
//! Recipes supersede templates for summarization. Each recipe is a markdown
//! file with YAML frontmatter that defines triggers (calendar keywords,
//! attendees), priority, and whether it's the fallback recipe. The body IS
//! the LLM prompt.
//!
//! Layout on disk:
//! ```text
//! ~/.minutes/
//! ├── profile.md          ← Shared context (who you are)
//! └── recipes/
//!     ├── manager-1on1.md
//!     ├── daily-standup.md
//!     └── working-session.md  ← fallback: true
//! ```

use crate::error::RecipeError;
use crate::markdown::split_frontmatter;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Recipe triggers for auto-selection based on calendar events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeTriggers {
    /// Match if any keyword appears in the calendar event title (case-insensitive).
    #[serde(default)]
    pub calendar_keywords: Vec<String>,
    /// Match if any attendee email is in the event.
    #[serde(default)]
    pub attendees: Vec<String>,
    /// Advanced: regex against event title.
    #[serde(default)]
    pub regex: Option<String>,
}

/// Recipe frontmatter fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeFrontmatter {
    /// Human-readable display name.
    pub name: String,
    /// Unique identifier (lowercase, hyphens).
    pub slug: String,
    /// Semver version string.
    pub version: String,
    /// One-liner for the recipe list UI.
    #[serde(default)]
    pub description: String,
    /// Auto-selection rules.
    #[serde(default)]
    pub triggers: RecipeTriggers,
    /// Higher = preferred when multiple recipes match (default: 0).
    #[serde(default)]
    pub priority: i32,
    /// If true, used when no other recipe matches.
    #[serde(default)]
    pub fallback: bool,
    /// Override summary language.
    #[serde(default)]
    pub language: Option<String>,
}

/// A loaded recipe: parsed frontmatter plus the prompt body.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub frontmatter: RecipeFrontmatter,
    /// The full prompt body (sent to LLM as-is).
    pub body: String,
    /// Path on disk.
    pub path: PathBuf,
}

/// Serializable recipe listing for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeListing {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub has_triggers: bool,
    pub is_fallback: bool,
    pub priority: i32,
    pub version: String,
}

/// User profile loaded from `~/.minutes/profile.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub focus: String,
    /// The prose body injected into prompts.
    #[serde(skip)]
    pub context: String,
}

impl Recipe {
    /// Parse a recipe from raw markdown source.
    pub fn from_str(source_text: &str, path: PathBuf) -> Result<Self, RecipeError> {
        let display = path.display().to_string();
        let (fm_text, body) = split_frontmatter(source_text);
        if fm_text.is_empty() {
            return Err(RecipeError::Invalid {
                path: display,
                message: "missing YAML frontmatter (file must start with '---')".into(),
            });
        }

        let frontmatter: RecipeFrontmatter =
            serde_yaml::from_str(fm_text).map_err(|e| RecipeError::Invalid {
                path: display.clone(),
                message: e.to_string(),
            })?;

        validate_slug(&frontmatter.slug, &display)?;

        Ok(Recipe {
            frontmatter,
            body: body.to_string(),
            path,
        })
    }

    /// Load and parse a recipe from a file.
    pub fn load_file(path: &Path) -> Result<Self, RecipeError> {
        let text = fs::read_to_string(path).map_err(|e| RecipeError::Io(e.to_string()))?;
        Self::from_str(&text, path.to_path_buf())
    }

    pub fn slug(&self) -> &str {
        &self.frontmatter.slug
    }

    /// Serialize this recipe back to markdown (frontmatter + body).
    pub fn to_markdown(&self) -> String {
        let fm = serde_yaml::to_string(&self.frontmatter).unwrap_or_default();
        format!("---\n{}---\n\n{}", fm, self.body)
    }

    /// Check if this recipe matches a calendar event.
    pub fn matches_event(&self, title: &str, attendee_emails: &[String]) -> bool {
        let title_lower = title.to_lowercase();
        // Check calendar keywords
        for kw in &self.frontmatter.triggers.calendar_keywords {
            if title_lower.contains(&kw.to_lowercase()) {
                return true;
            }
        }
        // Check attendees
        for attendee in &self.frontmatter.triggers.attendees {
            let attendee_lower = attendee.to_lowercase();
            if attendee_emails
                .iter()
                .any(|e| e.to_lowercase() == attendee_lower)
            {
                return true;
            }
        }
        false
    }
}

/// Manages the recipe library at `~/.minutes/recipes/`.
pub struct RecipeStore {
    recipes_dir: PathBuf,
    profile_path: PathBuf,
}

impl RecipeStore {
    /// Create a new store using the default paths.
    pub fn new() -> Self {
        let base = default_minutes_dir();
        Self {
            recipes_dir: base.join("recipes"),
            profile_path: base.join("profile.md"),
        }
    }

    /// Create with explicit paths (for testing).
    pub fn with_paths(recipes_dir: PathBuf, profile_path: PathBuf) -> Self {
        Self {
            recipes_dir,
            profile_path,
        }
    }

    /// Ensure the recipes directory exists.
    pub fn ensure_dir(&self) -> Result<(), RecipeError> {
        if !self.recipes_dir.exists() {
            fs::create_dir_all(&self.recipes_dir)
                .map_err(|e| RecipeError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// List all recipes.
    pub fn list(&self) -> Vec<RecipeListing> {
        let mut recipes = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.recipes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(recipe) = Recipe::load_file(&path) {
                        recipes.push(RecipeListing {
                            slug: recipe.frontmatter.slug.clone(),
                            name: recipe.frontmatter.name.clone(),
                            description: recipe.frontmatter.description.clone(),
                            has_triggers: !recipe.frontmatter.triggers.calendar_keywords.is_empty()
                                || !recipe.frontmatter.triggers.attendees.is_empty()
                                || recipe.frontmatter.triggers.regex.is_some(),
                            is_fallback: recipe.frontmatter.fallback,
                            priority: recipe.frontmatter.priority,
                            version: recipe.frontmatter.version.clone(),
                        });
                    }
                }
            }
        }
        recipes.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.slug.cmp(&b.slug)));
        recipes
    }

    /// Load a single recipe by slug.
    pub fn get(&self, slug: &str) -> Result<Recipe, RecipeError> {
        let path = self.recipes_dir.join(format!("{}.md", slug));
        if !path.exists() {
            return Err(RecipeError::NotFound(slug.to_string()));
        }
        Recipe::load_file(&path)
    }

    /// Save a recipe (create or overwrite).
    pub fn save(&self, recipe: &Recipe) -> Result<(), RecipeError> {
        self.ensure_dir()?;
        let path = self.recipes_dir.join(format!("{}.md", recipe.frontmatter.slug));
        let content = recipe.to_markdown();
        fs::write(&path, content).map_err(|e| RecipeError::Io(e.to_string()))?;
        // Set restrictive permissions (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Delete a recipe by slug.
    pub fn delete(&self, slug: &str) -> Result<(), RecipeError> {
        let path = self.recipes_dir.join(format!("{}.md", slug));
        if !path.exists() {
            return Err(RecipeError::NotFound(slug.to_string()));
        }
        fs::remove_file(&path).map_err(|e| RecipeError::Io(e.to_string()))?;
        Ok(())
    }

    /// Select the best recipe for a calendar event.
    pub fn select_for_event(
        &self,
        title: &str,
        attendee_emails: &[String],
    ) -> Option<RecipeListing> {
        let mut matches: Vec<(i32, RecipeListing)> = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.recipes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(recipe) = Recipe::load_file(&path) {
                        if recipe.matches_event(title, attendee_emails) {
                            matches.push((
                                recipe.frontmatter.priority,
                                RecipeListing {
                                    slug: recipe.frontmatter.slug.clone(),
                                    name: recipe.frontmatter.name.clone(),
                                    description: recipe.frontmatter.description.clone(),
                                    has_triggers: true,
                                    is_fallback: recipe.frontmatter.fallback,
                                    priority: recipe.frontmatter.priority,
                                    version: recipe.frontmatter.version.clone(),
                                },
                            ));
                        }
                    }
                }
            }
        }

        if let Some((_, listing)) = matches.into_iter().max_by_key(|(p, _)| *p) {
            return Some(listing);
        }

        // No match — return fallback recipe
        self.get_fallback()
    }

    /// Get the fallback recipe.
    fn get_fallback(&self) -> Option<RecipeListing> {
        if let Ok(entries) = fs::read_dir(&self.recipes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(recipe) = Recipe::load_file(&path) {
                        if recipe.frontmatter.fallback {
                            return Some(RecipeListing {
                                slug: recipe.frontmatter.slug.clone(),
                                name: recipe.frontmatter.name.clone(),
                                description: recipe.frontmatter.description.clone(),
                                has_triggers: false,
                                is_fallback: true,
                                priority: recipe.frontmatter.priority,
                                version: recipe.frontmatter.version.clone(),
                            });
                        }
                    }
                }
            }
        }
        None
    }

    /// Load the user profile.
    pub fn load_profile(&self) -> Result<Profile, RecipeError> {
        if !self.profile_path.exists() {
            return Err(RecipeError::ProfileNotFound);
        }
        let text =
            fs::read_to_string(&self.profile_path).map_err(|e| RecipeError::Io(e.to_string()))?;
        let (fm_text, body) = split_frontmatter(&text);
        if fm_text.is_empty() {
            return Err(RecipeError::Invalid {
                path: self.profile_path.display().to_string(),
                message: "profile.md must have YAML frontmatter".into(),
            });
        }
        let mut profile: Profile =
            serde_yaml::from_str(fm_text).map_err(|e| RecipeError::Invalid {
                path: self.profile_path.display().to_string(),
                message: e.to_string(),
            })?;
        profile.context = body.to_string();
        Ok(profile)
    }

    /// Save the user profile.
    pub fn save_profile(&self, profile: &Profile) -> Result<(), RecipeError> {
        let fm = serde_yaml::to_string(&ProfileFrontmatter {
            name: profile.name.clone(),
            role: profile.role.clone(),
            company: profile.company.clone(),
            team: profile.team.clone(),
            focus: profile.focus.clone(),
        })
        .unwrap_or_default();
        let content = format!("---\n{}---\n\n{}", fm, profile.context);
        fs::write(&self.profile_path, &content).map_err(|e| RecipeError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.profile_path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn recipes_dir(&self) -> &Path {
        &self.recipes_dir
    }
}

impl Default for RecipeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper struct for profile serialization (without the context body).
#[derive(Serialize)]
struct ProfileFrontmatter {
    name: String,
    role: String,
    company: String,
    team: String,
    focus: String,
}

fn default_minutes_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".minutes")
}

fn validate_slug(slug: &str, display_path: &str) -> Result<(), RecipeError> {
    if slug.is_empty() {
        return Err(RecipeError::InvalidSlug {
            path: display_path.to_string(),
            slug: slug.to_string(),
        });
    }
    let valid = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let bookended = !slug.starts_with('-') && !slug.ends_with('-');
    if !valid || !bookended {
        return Err(RecipeError::InvalidSlug {
            path: display_path.to_string(),
            slug: slug.to_string(),
        });
    }
    Ok(())
}
