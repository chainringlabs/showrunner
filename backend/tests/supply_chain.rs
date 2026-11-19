//! Checks on the backend's advisory ignore lists and on Renovate's version
//! holds. The files are read as they are, so the rules hold for whatever
//! entries they carry.

use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The value of `key = …` in a block of TOML lines, without surrounding quotes.
fn value_of<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"'))
    })
}

/// Each `[[IgnoredVulns]]` table in osv-scanner.toml, comments removed.
fn osv_entries(toml: &str) -> Vec<String> {
    let uncommented: String = toml
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    uncommented
        .split("[[IgnoredVulns]]")
        .skip(1)
        .map(str::to_owned)
        .collect()
}

/// The advisory ids in audit.toml's `ignore = [...]` array.
fn audit_ignored(toml: &str) -> Vec<String> {
    let uncommented: String = toml
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let Some(start) = uncommented.find("ignore") else {
        return Vec::new();
    };
    let rest = &uncommented[start..];
    let (open, close) = (
        rest.find('[').expect("ignore array"),
        rest.find(']').expect("ignore array end"),
    );
    rest[open + 1..close]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

// @spec SHIP-SUPPLY-005
#[test]
fn every_osv_ignore_has_a_reason_and_an_expiry() {
    for entry in osv_entries(&read("osv-scanner.toml")) {
        let id = value_of(&entry, "id").expect("entry without an id");
        let reason = value_of(&entry, "reason").unwrap_or_default();
        assert!(!reason.is_empty(), "{id}: no reason");
        let until = value_of(&entry, "effectiveUntil").unwrap_or_default();
        let parts: Vec<&str> = until.split('-').collect();
        assert!(
            parts.len() == 3
                && parts[0].len() == 4
                && parts[1].len() == 2
                && parts[2].len() == 2
                && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())),
            "{id}: effectiveUntil must be a YYYY-MM-DD date, got {until:?}"
        );
    }
}

// @spec SHIP-SUPPLY-005
#[test]
fn every_cargo_audit_ignore_is_also_an_osv_entry() {
    let osv_ids: Vec<String> = osv_entries(&read("osv-scanner.toml"))
        .iter()
        .filter_map(|e| value_of(e, "id").map(str::to_owned))
        .collect();
    for id in audit_ignored(&read(".cargo/audit.toml")) {
        assert!(
            osv_ids.contains(&id),
            "{id} is ignored for cargo audit but has no dated entry in osv-scanner.toml"
        );
    }
}

// @spec SHIP-SUPPLY-005
#[test]
fn the_parsers_see_entries_and_reject_a_missing_expiry() {
    let sample = "# [[IgnoredVulns]]\n[[IgnoredVulns]]\nid = \"X-1\"\nreason = \"r\"\n";
    let entries = osv_entries(sample);
    assert_eq!(entries.len(), 1);
    assert_eq!(value_of(&entries[0], "id"), Some("X-1"));
    assert_eq!(value_of(&entries[0], "effectiveUntil"), None);
    assert_eq!(
        audit_ignored("[advisories]\nignore = [\"A-1\", \"B-2\"]\n"),
        ["A-1", "B-2"]
    );
    assert!(audit_ignored("[advisories]\n").is_empty());
}

/// What is wrong with each `allowedVersions` rule in a Renovate config: one
/// line per rule that names no packages, or whose description has no link.
fn version_hold_problems(renovate_json: &str) -> Vec<String> {
    let config: serde_json::Value = serde_json::from_str(renovate_json).expect("renovate.json");
    let rules = config["packageRules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut problems = Vec::new();
    for rule in rules.iter().filter(|r| r.get("allowedVersions").is_some()) {
        if rule["matchPackageNames"]
            .as_array()
            .is_none_or(|names| names.is_empty())
        {
            problems.push(format!("{rule}: no matchPackageNames"));
        }
        let description = rule["description"].as_str().unwrap_or_default();
        if !description.contains("https://") {
            problems.push(format!("{rule}: description has no tracking link"));
        }
    }
    problems
}

// @spec SHIP-SUPPLY-006
#[test]
fn every_renovate_version_hold_is_named_and_explained() {
    let problems = version_hold_problems(&read("../renovate.json"));
    assert!(problems.is_empty(), "{problems:#?}");
}

// @spec SHIP-SUPPLY-006
#[test]
fn a_version_hold_without_packages_or_a_tracking_link_is_reported() {
    let bare = r#"{"packageRules": [{"allowedVersions": "<2"}, {"automerge": true}]}"#;
    assert_eq!(version_hold_problems(bare).len(), 2);
    let explained = r#"{"packageRules": [{"matchPackageNames": ["a"],
        "allowedVersions": "<2", "description": "b needs a 1.x: https://example.com/1"}]}"#;
    assert!(version_hold_problems(explained).is_empty());
}
