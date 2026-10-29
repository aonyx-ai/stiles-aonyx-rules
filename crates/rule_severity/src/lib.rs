//! Holds the test that keeps every rule at error severity
//!
//! A rule in this repository reports an error, or it is not a rule. A
//! finding that is allowed to stay accumulates, so the repository never
//! ships a warning. The test reads the sources under `lints/` and fails
//! when one of them names a severity other than `Severity::Error`.
//!
//! The test reads text rather than diagnostics. A rule builds its
//! diagnostics only for the source it reports on, so no single input
//! makes every rule speak. The source names every severity a rule can
//! report, whichever input triggers it.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;

    const SEVERITY: &str = "Severity::";

    const ERROR: &str = "Error";

    fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(directory).expect("the directory should be readable");
        for entry in entries {
            let path = entry.expect("the entry should be readable").path();
            match path.is_dir() {
                true => collect_rust_files(&path, files),
                false => {
                    if path.extension().is_some_and(|extension| extension == "rs") {
                        files.push(path);
                    }
                }
            }
        }
    }

    fn is_error(rest: &str) -> bool {
        let Some(after) = rest.strip_prefix(ERROR) else {
            return false;
        };
        !after
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || next == '_')
    }

    fn lint_sources() -> Vec<PathBuf> {
        let lints = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lints");
        let mut files = Vec::new();
        collect_rust_files(&lints, &mut files);
        files.sort();
        files
    }

    fn severities_other_than_error(file: &Path) -> Vec<String> {
        let source = fs::read_to_string(file).expect("the source should be readable");
        source
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                line.match_indices(SEVERITY)
                    .any(|(start, _)| !is_error(&line[start + SEVERITY.len()..]))
            })
            .map(|(index, line)| format!("{}:{}: {}", file.display(), index + 1, line.trim()))
            .collect()
    }

    #[test]
    fn is_error_with_a_longer_name_returns_false() {
        let rest = "ErrorOrWorse)";

        let error = is_error(rest);

        assert!(!error);
    }

    #[test]
    fn is_error_with_error_returns_true() {
        let rest = "Error)";

        let error = is_error(rest);

        assert!(error);
    }

    #[test]
    fn is_error_with_warn_returns_false() {
        let rest = "Warn)";

        let error = is_error(rest);

        assert!(!error);
    }

    #[test]
    fn lint_sources_finds_the_rules() {
        let files = lint_sources();

        assert!(!files.is_empty());
    }

    #[test]
    fn lint_sources_name_no_severity_other_than_error() {
        let files = lint_sources();

        let offenders: Vec<String> = files
            .iter()
            .flat_map(|file| severities_other_than_error(file))
            .collect();

        assert!(
            offenders.is_empty(),
            "every rule reports an error, but these lines name another severity:\n{}",
            offenders.join("\n")
        );
    }
}
