//! Search labels without changing their indices in the saved-session model.
pub(crate) fn matches(label: &str, query: &str) -> bool {
    label.to_lowercase().contains(&query.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn matches_names_hosts_and_unicode_without_case_sensitivity() {
        let label = "生产-Bastion (root@Host.Example:2222)";
        for query in [
            "BASTION",
            "host.example",
            "2222",
            "生产",
            "  root@host  ",
            "",
        ] {
            assert!(matches(label, query), "{query}");
        }
        assert!(!matches(label, "missing-host"));
    }
}
