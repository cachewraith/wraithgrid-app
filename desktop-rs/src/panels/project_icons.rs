//! Project icons (#80): `project:suggestIcons`, a port of src/shared/project-icons.ts
//! (`suggestProjectIcons`) and main's marker-file reading.

use std::collections::HashMap;
use std::path::Path;

const MARKER_FILES: [&str; 14] = [
    "composer.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pubspec.yaml",
    "Gemfile",
    "Dockerfile",
    "compose.yaml",
    "docker-compose.yml",
];

/// package.json dependencies → icon, first match wins (frameworks before their base).
const NODE_DEPS: [(&str, &str); 10] = [
    ("next", "dev:nextjs"),
    ("nuxt", "mit:nuxt"),
    ("@angular/core", "mit:angular"),
    ("svelte", "mit:svelte"),
    ("astro", "mit:astro"),
    ("electron", "dev:electron"),
    ("@tauri-apps/api", "mit:tauri"),
    ("vue", "mit:vue"),
    ("react", "mit:react"),
    ("vite", "mit:vite"),
];

/// The folder's top-level marker files, small ones read (a big one is present with no text).
pub fn read_markers(dir: &Path) -> HashMap<&'static str, String> {
    MARKER_FILES
        .iter()
        .filter_map(|f| {
            let p = dir.join(f);
            let meta = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            let text = if meta.len() <= 256 * 1024 {
                std::fs::read_to_string(&p).unwrap_or_default()
            } else {
                String::new()
            };
            Some((*f, text))
        })
        .collect()
}

/// Icons that fit a project, best first, from its marker files' text.
pub fn suggest(files: &HashMap<&str, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        if !out.iter().any(|o| o == id) {
            out.push(id.into());
        }
    };
    let has = |f: &str| files.contains_key(f);
    let text = |f: &str| files.get(f).map_or("", String::as_str);
    if has("composer.json") {
        add(if text("composer.json").contains("laravel/framework") {
            "mit:laravel"
        } else {
            "mit:php"
        });
    }
    let jvm = format!(
        "{}{}{}",
        text("pom.xml"),
        text("build.gradle"),
        text("build.gradle.kts")
    );
    if jvm.contains("spring-boot") || jvm.contains("org.springframework") {
        add("dev:spring");
    }
    if text("build.gradle.kts").contains("kotlin") || text("build.gradle").contains("kotlin") {
        add("mit:kotlin");
    }
    if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
        add("mit:java");
    }
    if has("package.json") {
        let pkg: serde_json::Value = serde_json::from_str(text("package.json")).unwrap_or_default();
        let dep = |name: &str| {
            ["devDependencies", "dependencies"]
                .iter()
                .any(|k| pkg[k].as_object().is_some_and(|o| o.contains_key(name)))
        };
        for (name, id) in NODE_DEPS {
            if dep(name) {
                add(id);
            }
        }
        add(if dep("typescript") {
            "mit:typescript"
        } else {
            "mit:nodejs"
        });
    }
    if has("Cargo.toml") {
        add("mit:rust");
    }
    if has("go.mod") {
        add("mit:go");
    }
    let py = format!("{}{}", text("pyproject.toml"), text("requirements.txt")).to_lowercase();
    if py.contains("django") {
        add("mit:django");
    }
    if has("pyproject.toml") || has("requirements.txt") {
        add("mit:python");
    }
    if has("pubspec.yaml") {
        add("dev:flutter");
    }
    if text("Gemfile").contains("rails") {
        add("mit:ruby");
    }
    if has("Dockerfile") || has("compose.yaml") || has("docker-compose.yml") {
        add("mit:docker");
    }
    out
}
