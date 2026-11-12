//! tests/unit/themes.test.ts: the built-ins, theme file validation, CSS variables, accents
//! and the themes folder. Plus the zod error texts and the file format, which the Electron
//! app shares with this one.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use wraithgrid_themes::{
    built_in_themes, delete_theme, import_theme, list_themes, parse_theme_file, pick_theme,
    save_theme, theme_file_text, theme_slug, theme_terminal, theme_vars, with_accent, Accent,
    TerminalColor, ThemeColor, ThemeName,
};

fn minimal() -> Value {
    json!({
        "name": "Ocean",
        "mode": "dark",
        "colors": { "bg": "#0f1a24", "text": "#E6F1FF", "accent": "#38bdf8" }
    })
}

/// `{ ...a, ...b }` for JSON objects.
fn with(mut a: Value, b: Value) -> Value {
    for (k, v) in b.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

fn with_colors(extra: Value) -> Value {
    let colors = with(minimal()["colors"].clone(), extra);
    with(minimal(), json!({ "colors": colors }))
}

#[test]
fn built_ins_are_five_with_every_color_and_all_terminal_colors_set() {
    let ids: Vec<&str> = built_in_themes().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["wraith", "midnight", "clay", "daylight", "paper"]);
    for t in built_in_themes() {
        let v = serde_json::to_value(&t.theme).unwrap();
        assert!(parse_theme_file(&v).is_ok(), "{}", t.id);
        for &k in ThemeColor::ALL {
            assert!(t.colors.get(k).is_some(), "{} {}", t.id, k.key());
        }
        let term = t.terminal.as_ref().unwrap();
        let set = TerminalColor::ALL
            .iter()
            .filter(|&&k| term.get(k).is_some());
        assert_eq!(set.count(), 20, "{}", t.id);
    }
}

#[test]
fn parse_accepts_three_colors_and_normalizes_hex() {
    let t = parse_theme_file(&minimal()).unwrap();
    assert_eq!(t.colors.text, "#e6f1ff");
    let t = parse_theme_file(&with_colors(json!({ "ok": "#0f0" }))).unwrap();
    assert_eq!(t.colors.ok.as_deref(), Some("#00ff00"));
}

#[test]
fn parse_refuses_anything_that_is_not_a_hex_color_so_no_css_gets_in() {
    for bad in [
        "red",
        "rgb(0,0,0)",
        "#000; background: url(https://x)",
        "var(--bg)",
        "#12345",
        "expression(alert(1))",
    ] {
        assert!(
            parse_theme_file(&with_colors(json!({ "bg": bad }))).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn parse_names_the_problem_missing_colors_unknown_keys_bad_mode_or_name() {
    let e = parse_theme_file(&with(minimal(), json!({ "colors": { "bg": "#000000" } })));
    assert!(e.unwrap_err().contains("colors.text"));
    assert!(parse_theme_file(&with_colors(json!({ "bgg": "#000" }))).is_err());
    assert!(parse_theme_file(&with(minimal(), json!({ "mode": "dim" }))).is_err());
    assert!(parse_theme_file(&with(minimal(), json!({ "name": "<script>" }))).is_err());
    assert!(parse_theme_file(&with(minimal(), json!({ "terminal": { "red": "blue" } }))).is_err());
}

#[test]
fn parse_errors_read_as_zod_words_them() {
    // Texts taken from zod 4.6.5 on the TS schema; the UI shows them as they are.
    let err = |v: Value| parse_theme_file(&v).unwrap_err();
    assert_eq!(
        err(json!(5)),
        "Invalid input: expected object, received number"
    );
    assert_eq!(
        err(json!([])),
        "Invalid input: expected object, received array"
    );
    assert_eq!(
        err(json!({})),
        "name: Invalid input: expected string, received undefined"
    );
    assert_eq!(
        err(with(minimal(), json!({ "name": "  " }))),
        "name: Too small: expected string to have >=1 characters"
    );
    assert_eq!(
        err(with(minimal(), json!({ "name": "x".repeat(41) }))),
        "name: Too big: expected string to have <=40 characters"
    );
    // Characters, not UTF-16 units: 21 emoji are 42 units and pass.
    assert!(parse_theme_file(&with(minimal(), json!({ "name": "😀".repeat(21) }))).is_ok());
    assert_eq!(
        err(with(minimal(), json!({ "name": "<s>" }))),
        "name: has characters a name cannot use"
    );
    assert_eq!(
        err(with(minimal(), json!({ "mode": "dim" }))),
        "mode: Invalid option: expected one of \"dark\"|\"light\""
    );
    assert_eq!(
        err(with(minimal(), json!({ "author": "x".repeat(61) }))),
        "author: Too big: expected string to have <=60 characters"
    );
    assert_eq!(
        err(with(minimal(), json!({ "author": "a<" }))),
        "author: Invalid string: must match pattern /^[^\\p{Cc}<>]*$/u"
    );
    assert_eq!(
        err(with(minimal(), json!({ "author": null }))),
        "author: Invalid input: expected string, received null"
    );
    assert_eq!(
        err(with_colors(json!({ "bg": "red" }))),
        "colors.bg: must be a hex color like #1f2937"
    );
    assert_eq!(
        err(with_colors(json!({ "bg": 1 }))),
        "colors.bg: Invalid input: expected string, received number"
    );
    assert_eq!(
        err(with_colors(json!({ "bgg": "#000", "zz": "#000" }))),
        "colors: Unrecognized keys: \"bgg\", \"zz\""
    );
    assert_eq!(
        err(with(minimal(), json!({ "terminal": { "x": "#000" } }))),
        "terminal: Unrecognized key: \"x\""
    );
    assert_eq!(
        err(with(minimal(), json!({ "terminal": null }))),
        "terminal: Invalid input: expected object, received null"
    );
    let style = |s: Value| err(with(minimal(), json!({ "style": s })));
    assert_eq!(
        style(json!({ "font": "Comic" })),
        "style.font: Invalid option: expected one of \"Geist\"|\"System\"|\"Inter\"|\"IBM Plex Sans\"|\"Source Sans 3\"|\"Noto Sans\""
    );
    assert_eq!(
        style(json!({ "chatTextSize": 30 })),
        "style.chatTextSize: Too big: expected number to be <=17"
    );
    assert_eq!(
        style(json!({ "chatTextSize": 3 })),
        "style.chatTextSize: Too small: expected number to be >=13"
    );
    assert_eq!(
        style(json!({ "chatTextSize": 14.5 })),
        "style.chatTextSize: Invalid input: expected int, received number"
    );
    assert_eq!(
        style(json!({ "chatTextSize": "14" })),
        "style.chatTextSize: Invalid input: expected number, received string"
    );
    assert_eq!(
        style(json!({ "frameGradient": "yes" })),
        "style.frameGradient: Invalid input: expected boolean, received string"
    );
    assert_eq!(
        style(json!({ "css": "x" })),
        "style: Unrecognized key: \"css\""
    );
    // The first problem in schema order wins.
    assert_eq!(
        err(json!({ "name": 5, "mode": "x", "colors": { "bg": "red" } })),
        "name: Invalid input: expected string, received number"
    );
    // A whole number written as a float is a whole number.
    let t = parse_theme_file(&with(
        minimal(),
        json!({ "style": { "chatTextSize": 14.0 } }),
    ));
    assert_eq!(t.unwrap().style.unwrap().chat_text_size, Some(14));
}

#[test]
fn names_are_trimmed_with_js_white_space() {
    let t = parse_theme_file(&with(minimal(), json!({ "name": "\u{feff} Ocean \u{a0}" })));
    assert_eq!(t.unwrap().name, "Ocean");
    // U+0085 is not white space to JS, and it is a control character.
    assert!(parse_theme_file(&with(minimal(), json!({ "name": "Ocean\u{85}" }))).is_err());
}

#[test]
fn theme_vars_fill_every_token_for_a_three_color_theme_from_its_own_colors() {
    let t = parse_theme_file(&minimal()).unwrap();
    let v = theme_vars(&t);
    for &c in ThemeColor::ALL {
        assert!(
            !v.get(c.css_var()).unwrap_or("").is_empty(),
            "{}",
            c.css_var()
        );
    }
    assert_eq!(v.get("--bg"), Some("#0f1a24"));
    assert!(v.get("--line").unwrap().contains("var(--bg)"));
    let term = theme_terminal(&t);
    assert_eq!(term.background, "#0f1a24");
    assert_eq!(term.cursor, "#38bdf8");
}

#[test]
fn an_accent_replaces_the_accent_colors_only() {
    let w = &built_in_themes()[0];
    let amber = with_accent(w, Accent::Amber);
    assert_eq!(amber.colors.accent, "#f59e0b");
    assert_eq!(amber.colors.bg, w.colors.bg);
    assert_eq!(&with_accent(w, Accent::Theme), &w.theme);
}

#[test]
fn pick_theme_falls_back_to_the_mode_default_when_the_id_is_gone_or_the_wrong_mode() {
    let all = built_in_themes();
    assert_eq!(pick_theme(all, "clay", ThemeName::Dark).id, "clay");
    assert_eq!(pick_theme(all, "gone", ThemeName::Dark).id, "wraith");
    assert_eq!(pick_theme(all, "paper", ThemeName::Dark).id, "wraith");
    assert_eq!(pick_theme(all, "gone", ThemeName::Light).id, "daylight");
}

#[test]
fn slugs_names_into_file_ids() {
    assert_eq!(theme_slug("My Ocean!"), "my-ocean");
    assert_eq!(theme_slug("…"), "theme");
    assert_eq!(theme_slug("Café Noir"), "cafe-noir");
    assert_eq!(theme_slug(&"ab ".repeat(30)).len(), 40);
}

// ---- themes folder ----------------------------------------------------------------

struct Tmp {
    _root: tempfile::TempDir,
    dir: PathBuf,
    src: PathBuf,
}

fn tmp() -> Tmp {
    let root = tempfile::Builder::new()
        .prefix("wg-themes-")
        .tempdir()
        .unwrap();
    let dir = root.path().join("themes");
    let src = root.path().join("src");
    fs::create_dir(&src).unwrap();
    Tmp {
        _root: root,
        dir,
        src,
    }
}

#[test]
fn imports_a_valid_file_under_a_free_id_and_lists_it() {
    let t = tmp();
    let f = t.src.join("whatever.json");
    fs::write(
        &f,
        with(minimal(), json!({ "extra": "dropped" })).to_string(),
    )
    .unwrap();
    assert_eq!(import_theme(&f, &t.dir).unwrap(), Ok("ocean".into()));
    assert_eq!(import_theme(&f, &t.dir).unwrap(), Ok("ocean-2".into()));
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(t.dir.join("ocean.json")).unwrap()).unwrap();
    assert!(saved.get("extra").is_none());
    let list = list_themes(&t.dir);
    let ids: Vec<&str> = list.themes.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["ocean", "ocean-2"]);
    assert!(!list.themes[0].built_in);
    assert_eq!(list.themes[0].colors.text, "#e6f1ff");
}

#[test]
fn refuses_bad_files_on_import_and_reports_them_in_the_list() {
    let t = tmp();
    let f = t.src.join("bad.json");
    fs::write(&f, with_colors(json!({ "bg": "url(x)" })).to_string()).unwrap();
    let e = import_theme(&f, &t.dir).unwrap().unwrap_err();
    assert_eq!(e, "bad.json: colors.bg: must be a hex color like #1f2937");
    fs::create_dir_all(&t.dir).unwrap();
    fs::write(t.dir.join("broken.json"), "{nope").unwrap();
    fs::write(t.dir.join("wraith.json"), minimal().to_string()).unwrap();
    fs::write(t.dir.join("Bad Name.json"), minimal().to_string()).unwrap();
    let list = list_themes(&t.dir);
    assert!(list.themes.is_empty());
    let mut files: Vec<(&str, &str)> = list
        .problems
        .iter()
        .map(|p| (p.file.as_str(), p.error.as_str()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            (
                "Bad Name.json",
                "file name must be lowercase letters, digits and -"
            ),
            ("broken.json", "not valid JSON"),
            ("wraith.json", "a built-in theme has this name"),
        ]
    );
}

#[test]
fn import_of_a_missing_file_says_so_in_nodes_words() {
    let t = tmp();
    let f = t.src.join("gone.json");
    let e = import_theme(&f, &t.dir).unwrap().unwrap_err();
    assert_eq!(
        e,
        format!("ENOENT: no such file or directory, stat '{}'", f.display())
    );
}

#[test]
fn a_large_file_or_a_folder_is_a_problem_not_a_theme() {
    let t = tmp();
    fs::create_dir_all(t.dir.join("folder.json")).unwrap();
    fs::write(t.dir.join("big.json"), " ".repeat(64 * 1024 + 1)).unwrap();
    let list = list_themes(&t.dir);
    let errs: Vec<(&str, &str)> = list
        .problems
        .iter()
        .map(|p| (p.file.as_str(), p.error.as_str()))
        .collect();
    assert_eq!(
        errs,
        [
            ("big.json", "larger than 64 KB"),
            ("folder.json", "not a file")
        ]
    );
}

#[test]
fn a_missing_folder_lists_nothing() {
    let t = tmp();
    let list = list_themes(&t.dir);
    assert!(list.themes.is_empty() && list.problems.is_empty());
}

#[test]
fn deletes_only_its_own_theme_files() {
    let t = tmp();
    let f = t.src.join("o.json");
    fs::write(&f, minimal().to_string()).unwrap();
    import_theme(&f, &t.dir).unwrap().unwrap();
    assert!(!delete_theme("wraith", &t.dir).unwrap());
    assert!(!delete_theme("../x", &t.dir).unwrap());
    assert!(delete_theme("ocean", &t.dir).unwrap());
    assert!(!t.dir.join("ocean.json").exists());
}

#[test]
fn saves_the_editor_theme_after_checking_it_under_a_free_id() {
    let t = tmp();
    let theme = with(minimal(), json!({ "style": { "radius": "large" } }));
    assert_eq!(save_theme(&theme, &t.dir).unwrap(), Ok("ocean".into()));
    assert_eq!(save_theme(&theme, &t.dir).unwrap(), Ok("ocean-2".into()));
    let r = list_themes(&t.dir);
    let radii: Vec<_> = r
        .themes
        .iter()
        .map(|t| t.style.as_ref().and_then(|s| s.radius).map(|r| r.as_str()))
        .collect();
    assert_eq!(radii, [Some("large"), Some("large")]);
    let bad = save_theme(
        &with(minimal(), json!({ "style": { "radius": "huge" } })),
        &t.dir,
    )
    .unwrap();
    assert!(bad.is_err());
    assert_eq!(list_themes(&t.dir).themes.len(), 2);
}

#[test]
fn writes_the_file_as_the_electron_app_does() {
    // JSON.stringify(parsed, null, 2) + "\n" for a file given out of order with extras.
    let t = tmp();
    let raw = json!({
        "style": { "frameGradient": true, "chatTextSize": 16.0, "font": "Inter" },
        "colors": { "accent": "#ABC", "diffAdd": "#00FF0022", "text": "#eeeeee", "bg": "#101010" },
        "terminal": {},
        "extra": 1,
        "author": "  ana ",
        "mode": "dark",
        "name": " Mine "
    });
    assert_eq!(save_theme(&raw, &t.dir).unwrap(), Ok("mine".into()));
    let text = fs::read_to_string(t.dir.join("mine.json")).unwrap();
    assert_eq!(
        text,
        r##"{
  "name": "Mine",
  "mode": "dark",
  "author": "ana",
  "colors": {
    "bg": "#101010",
    "text": "#eeeeee",
    "accent": "#aabbcc",
    "diffAdd": "#00ff0022"
  },
  "terminal": {},
  "style": {
    "font": "Inter",
    "chatTextSize": 16,
    "frameGradient": true
  }
}
"##
    );
    // And it reads back the same.
    let back = list_themes(&t.dir).themes.remove(0);
    assert_eq!(theme_file_text(&back), text);
}

// ---- theme style (#24) ----------------------------------------------------------------

fn base() -> Value {
    json!({
        "name": "Mine",
        "mode": "dark",
        "colors": { "bg": "#101010", "text": "#eeeeee", "accent": "#ff8800" }
    })
}

#[test]
fn takes_the_style_keys_and_the_new_chat_colors() {
    let colors = with(
        base()["colors"].clone(),
        json!({ "userBubble": "#223344", "codeBlock": "#0a0a0a", "diffAdd": "#00ff0022" }),
    );
    let t = parse_theme_file(&with(
        base(),
        json!({
            "colors": colors,
            "style": {
                "font": "Inter",
                "monoFont": "Fira Code",
                "chatTextSize": 16,
                "radius": "large",
                "border": "thick",
                "depth": "deep",
                "density": "compact",
                "motion": "reduced",
                "frameGradient": true
            }
        }),
    ))
    .unwrap();
    let v = theme_vars(&t);
    assert!(v.get("--font-ui").unwrap().contains("'Inter'"));
    assert!(v.get("--font-mono").unwrap().contains("'Fira Code'"));
    assert_eq!(v.get("--chat-size"), Some("16px"));
    assert_eq!(v.get("--rs"), Some("1.6"));
    assert_eq!(v.get("--bw"), Some("1.5px"));
    assert_eq!(v.get("--dens"), Some("0.85"));
    assert!(v.get("--frame-img").unwrap().contains("radial-gradient"));
    assert_eq!(v.get("--user-bg"), Some("#223344"));
    assert_eq!(v.get("--addbg"), Some("#00ff0022"));
}

#[test]
fn refuses_values_outside_the_fixed_options_and_raw_css() {
    for style in [
        json!({ "radius": "huge" }),
        json!({ "font": "Comic Sans MS" }),
        json!({ "chatTextSize": 30 }),
        json!({ "motion": "wild" }),
        json!({ "css": "body { display: none }" }),
        json!({ "font": "Geist'; } body { x: y" }),
    ] {
        let r = parse_theme_file(&with(base(), json!({ "style": style.clone() })));
        assert!(r.is_err(), "{style}");
    }
    let colors = with(base()["colors"].clone(), json!({ "userBubble": "red" }));
    assert!(parse_theme_file(&with(base(), json!({ "colors": colors }))).is_err());
}

#[test]
fn an_old_file_without_a_style_keeps_loading_with_the_default_look() {
    let t = parse_theme_file(&base()).unwrap();
    assert!(t.style.is_none());
    let v = theme_vars(&t);
    assert_eq!(v.get("--rs"), Some("1.2"));
    assert!(v.get("--font-ui").unwrap().contains("'Geist'"));
    assert_eq!(v.get("--frame-img"), Some("none"));
    assert!(v.get("--user-bg").unwrap().contains("color-mix"));
    assert_eq!(v.get("--chat-size"), Some("14.5px"));
    assert_eq!(v.get("--dens"), Some("1"));
}

#[test]
fn an_accent_override_re_tints_the_user_bubble() {
    let t = with_accent(&built_in_themes()[0], Accent::Blue);
    assert!(t.colors.user_bubble.is_none());
    assert!(theme_vars(&t)
        .get("--user-bg")
        .unwrap()
        .contains("var(--acc)"));
}

#[test]
fn the_built_ins_differ_in_style_not_only_color_and_all_validate() {
    let mut looks: Vec<String> = built_in_themes()
        .iter()
        .map(|t| serde_json::to_string(&t.style).unwrap())
        .collect();
    looks.sort();
    looks.dedup();
    assert!(looks.len() >= 4);
    for t in built_in_themes() {
        let file = serde_json::to_value(&t.theme).unwrap();
        assert!(parse_theme_file(&file).is_ok(), "{}", t.id);
    }
}

#[test]
fn every_built_in_has_the_same_corner_radius_so_switching_changes_colours_not_shape() {
    for t in built_in_themes() {
        assert_eq!(theme_vars(t).get("--rs"), Some("1.2"), "{}", t.id);
    }
}
