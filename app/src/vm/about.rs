//! About view model: upstream credits, copied project credits, licences,
//! Slint attribution notice, and list of fixes offered upstream (SPEC.md M8).

use std::path::Path;

/// One project credited in About.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCredit {
    pub name: &'static str,
    pub licence: &'static str,
    pub role: &'static str,
}

/// Upstream project credit.
pub const UPSTREAM_CREDIT: ProjectCredit = ProjectCredit {
    name: "Nycolazs/ACNHRecomp",
    licence: "MIT",
    role: "Upstream: recompilation, runtime, launcher, and releases",
};

// This list mirrors the project table in ROADMAP.md.
/// Copied and referenced open-source projects credited with their licences.
pub const COPIED_PROJECTS: &[ProjectCredit] = &[
    ProjectCredit {
        name: "iannitram/totk-universal",
        licence: "MIT",
        role: "Launcher architecture, Continue screen brief, core modules",
    },
    ProjectCredit {
        name: "iannitram/save-relay",
        licence: "MIT",
        role: "Cross-device save synchronization",
    },
    ProjectCredit {
        name: "cesar527/ACNHRecomp",
        licence: "MIT",
        role: "Thermal governor and handheld power modes",
    },
    ProjectCredit {
        name: "ZUlxian/ACNHRecomp",
        licence: "MIT",
        role: "Game folder import, Windows port and 16:10 fixes",
    },
    ProjectCredit {
        name: "fedebuonco/nxrt, nxrt-frontend",
        licence: "Unlicense",
        role: "Learned from, not copied: one shared runtime; launcher prior art",
    },
    ProjectCredit {
        name: "slint-ui/slint",
        licence: "Royalty-Free (Attribution)",
        role: "Desktop and mobile user interface framework",
    },
];

/// Slint attribution notice required by its royalty-free licence (ADR 0003).
pub const SLINT_NOTICE: &str =
    "GUI powered by Slint (https://slint.dev). Copyright © SixtyFPS GmbH. Licensed under the Slint Royalty-Free License.";

/// Default embedded data for fixes of ours offered upstream.
pub const DEFAULT_OFFERED_FIXES: &str = include_str!("offered_fixes.txt");

/// One fix of ours offered upstream (SPEC.md M8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedFix {
    pub title: String,
    pub description: String,
    pub link: String,
    pub status: String,
}

impl OfferedFix {
    /// Whether upstream already has this fix.
    pub fn upstream_has_it(&self) -> bool {
        matches!(
            self.status.to_ascii_lowercase().as_str(),
            "upstream" | "landed" | "merged" | "done"
        )
    }

    /// Whether this fix is still offered (pending upstream merge).
    pub fn is_offered(&self) -> bool {
        !self.upstream_has_it()
    }
}

/// Parses the offered fixes text data.
pub fn parse_offered_fixes(text: &str) -> Vec<OfferedFix> {
    let mut fixes = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let (title, desc, link, status) = match parts.len() {
                2 => (parts[0], parts[0], parts[1], "offered"),
                3 => (parts[0], parts[0], parts[1], parts[2]),
                _ => (parts[0], parts[1], parts[2], parts[3]),
            };
            fixes.push(OfferedFix {
                title: title.to_owned(),
                description: desc.to_owned(),
                link: link.to_owned(),
                status: status.to_owned(),
            });
        }
    }
    fixes
}

/// Reads offered fixes from a file path.
pub fn read_offered_fixes(path: &Path) -> Result<Vec<OfferedFix>, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_offered_fixes(&text))
}

/// View model for the About page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AboutVm {
    upstream: ProjectCredit,
    credits: Vec<ProjectCredit>,
    fixes: Vec<OfferedFix>,
    slint_notice: String,
}

impl Default for AboutVm {
    fn default() -> Self {
        Self::new()
    }
}

impl AboutVm {
    pub fn new() -> Self {
        Self {
            upstream: UPSTREAM_CREDIT,
            credits: COPIED_PROJECTS.to_vec(),
            fixes: parse_offered_fixes(DEFAULT_OFFERED_FIXES),
            slint_notice: SLINT_NOTICE.to_owned(),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, std::io::Error> {
        let fixes = read_offered_fixes(path)?;
        Ok(Self {
            upstream: UPSTREAM_CREDIT,
            credits: COPIED_PROJECTS.to_vec(),
            fixes,
            slint_notice: SLINT_NOTICE.to_owned(),
        })
    }

    pub fn upstream(&self) -> &ProjectCredit {
        &self.upstream
    }

    pub fn credits(&self) -> &[ProjectCredit] {
        &self.credits
    }

    pub fn all_fixes(&self) -> &[OfferedFix] {
        &self.fixes
    }

    /// List of fixes of ours offered upstream, shown until upstream has them (SPEC.md M8).
    pub fn offered_fixes(&self) -> Vec<&OfferedFix> {
        self.fixes.iter().filter(|f| f.is_offered()).collect()
    }

    pub fn slint_notice(&self) -> &str {
        &self.slint_notice
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_view_model_reads_offered_fixes_file_and_checks_slint_notice() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixes_file = manifest_dir.join("src/vm/offered_fixes.txt");

        let vm = AboutVm::from_file(&fixes_file).expect("should read offered-fixes file");

        // The Slint notice must be present
        assert!(!vm.slint_notice().is_empty());
        assert!(vm.slint_notice().contains("Slint"));
        assert!(vm.slint_notice().contains("https://slint.dev"));

        // Upstream and copied projects credited
        assert_eq!(vm.upstream().name, "Nycolazs/ACNHRecomp");
        assert_eq!(vm.upstream().licence, "MIT");
        assert!(vm
            .credits()
            .iter()
            .any(|c| c.name == "iannitram/totk-universal"));
        assert!(vm
            .credits()
            .iter()
            .any(|c| c.name == "iannitram/save-relay"));
        assert!(vm.credits().iter().any(|c| c.name == "slint-ui/slint"));

        // The shipped file has no entries until real upstream PRs exist.
        assert!(vm.offered_fixes().is_empty());
        assert!(!vm.credits().iter().any(|c| c.name.contains("Norviah")));
    }

    #[test]
    fn test_parse_offered_fixes_fixture() {
        // Fixture only; not real data.
        let text = "# c\nExample fix | https://example.invalid/pr/1 | offered\n\
                    Landed example | https://example.invalid/pr/2 | upstream\n";
        let fixes = parse_offered_fixes(text);
        assert_eq!(fixes.len(), 2);
        assert_eq!(fixes[0].link, "https://example.invalid/pr/1");
        assert!(fixes[0].is_offered());
        assert!(fixes[1].upstream_has_it());
    }

    #[test]
    fn test_default_view_model_matches_parsed_fixes() {
        let vm = AboutVm::default();
        assert!(vm.offered_fixes().is_empty());
        assert!(vm.slint_notice().contains("Slint"));
    }
}
