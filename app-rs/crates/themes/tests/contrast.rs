//! tests/unit/contrast.test.ts: WCAG ratios and the theme editor's contrast hints.

use wraithgrid_themes::{
    built_in_themes, contrast_issues, contrast_ratio, parse_theme_file, passing_color, ThemeColor,
};

#[test]
fn measures_wcag_ratios() {
    assert!((contrast_ratio("#000000", "#ffffff") - 21.0).abs() < 0.5);
    assert!((contrast_ratio("#ffffff", "#ffffff") - 1.0).abs() < 1e-5);
    assert!(contrast_ratio("#767676", "#ffffff") >= 4.5);
}

#[test]
fn finds_a_nearby_color_that_passes() {
    let fix = passing_color("#7a6f64", "#1f1b18", 4.5);
    assert!(contrast_ratio(&fix, "#1f1b18") >= 4.5);
    assert!(contrast_ratio(&passing_color("#cccccc", "#ffffff", 4.5), "#ffffff") >= 4.5);
}

#[test]
fn flags_only_the_themes_own_pairs_that_fall_short_with_a_fix() {
    let t = parse_theme_file(&serde_json::json!({
        "name": "x",
        "mode": "dark",
        "colors": { "bg": "#1f1b18", "text": "#f0e9e1", "accent": "#d97757", "faint": "#3a332d" }
    }))
    .unwrap();
    let issues = contrast_issues(&t);
    let fgs: Vec<ThemeColor> = issues.iter().map(|i| i.fg).collect();
    assert_eq!(fgs, [ThemeColor::Faint]);
    assert_eq!(issues[0].label, "Hints and timestamps on the background");
    assert_eq!(issues[0].need, 4.5);
    assert!(contrast_ratio(&issues[0].fix, "#1f1b18") >= 4.5);
    // Rounded down to one decimal.
    assert_eq!(issues[0].ratio, (issues[0].ratio * 10.0).floor() / 10.0);
}

#[test]
fn the_built_ins_pass_for_body_text() {
    for t in built_in_themes() {
        let text: Vec<_> = contrast_issues(t)
            .into_iter()
            .filter(|i| i.fg == ThemeColor::Text)
            .collect();
        assert!(text.is_empty(), "{}", t.id);
    }
}

#[test]
fn the_fix_is_the_same_color_the_ts_picks() {
    // Values from src/shared/contrast.ts run in node.
    assert_eq!(passing_color("#7a6f64", "#1f1b18", 4.5), "#8e857b");
    assert_eq!(passing_color("#cccccc", "#ffffff", 4.5), "#707070");
    assert_eq!(passing_color("#3a332d", "#1f1b18", 4.5), "#898581");
    assert_eq!(passing_color("#ff8800", "#ffffff", 7.0), "#804400");
}
