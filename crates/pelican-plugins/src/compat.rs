//! `Plugin::isCompatible()`: exact or composer-like caret constraints on the panel version.

use pelican_core::php::version_compare;

use crate::Plugin;

pub fn is_compatible(p: &Plugin, current: Option<&str>) -> bool {
    let (Some(constraint), Some(current)) = (p.panel_version.as_deref().filter(|c| !c.is_empty()), current) else {
        return true;
    };
    constraint_allows(constraint, current)
}

/// `^X.Y.Z` = `>= X.Y.Z` and below the next major (next minor for 0.x, next patch for
/// 0.0.x); anything else must equal the current version.
pub fn constraint_allows(constraint: &str, current: &str) -> bool {
    let Some(caret) = constraint.strip_prefix('^') else {
        return version_compare(current, constraint) == std::cmp::Ordering::Equal;
    };
    let parts: Vec<&str> = caret.trim_start_matches('^').split('.').collect();
    let mut padded: Vec<&str> = parts.clone();
    while padded.len() < 3 {
        padded.push("0");
    }
    let minimum = padded.join(".");
    let int = |s: &str| pelican_core::php::intval(s);
    let upper = if parts[0] != "0" || parts.len() < 2 {
        format!("{}.0.0", int(parts[0]) + 1)
    } else if parts[1] != "0" || parts.len() < 3 {
        format!("0.{}.0", int(parts[1]) + 1)
    } else {
        format!("0.0.{}", int(parts[2]) + 1)
    };
    version_compare(current, &minimum) != std::cmp::Ordering::Less
        && version_compare(current, &upper) == std::cmp::Ordering::Less
}

#[cfg(test)]
mod tests {
    use super::constraint_allows;

    /// Rows of the original's `tests/Unit/Models/PluginCompatibilityTest.php`.
    #[test]
    fn caret_and_exact() {
        for (constraint, current, ok) in [
            ("1.0.0", "1.0.0", true),
            ("1.0.0", "1.0.1", false),
            ("^1.0", "1.0.0", true),
            ("^1.0", "1.9.9", true),
            ("^1.0", "2.0.0", false),
            ("^1.0", "0.9.9", false),
            ("^1.2.3", "1.2.2", false),
            ("^1.2.3", "1.2.3", true),
            ("^0.2", "0.2.5", true),
            ("^0.2", "0.3.0", false),
            ("^0.0.3", "0.0.3", true),
            ("^0.0.3", "0.0.4", false),
            ("^1", "1.5.0", true),
            ("^1", "2.0.0", false),
            ("^0", "0.9.0", true),
            ("^0.0", "0.0.9", true),
            ("^0.0", "0.1.0", false),
            ("^1.2.3", "1.2.4", true),
            ("^1.0", "1.0.0-rc1", false),
            ("^1.0", "2.0.0-rc1", true),
        ] {
            assert_eq!(constraint_allows(constraint, current), ok, "{constraint} vs {current}");
        }
    }
}
