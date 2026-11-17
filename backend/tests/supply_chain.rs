//! Checks on the backend's advisory ignore lists. They are read as text, so
//! the rules hold for whatever entries the files carry.

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
