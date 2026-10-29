use super::*;

pub(super) fn cs(project: &str) -> Codestream {
    Codestream {
        project: project.into(),
        package: "curl".into(),
        devel: None,
    }
}

pub(super) const CHANGES: &str =
    "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 1.2.3:
  * fix a crash
  * new option

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 1.2.2 (bsc#1111111)
";

pub(super) fn request() -> MaintenanceRequest {
    MaintenanceRequest {
        target_project: "SUSE:Maintenance".into(),
        actions: vec![
            IncidentAction {
                source_project: "home:u:branches:SUSE:SLE-15-SP7:Update".into(),
                source_package: "curl".into(),
                release_project: "SUSE:SLE-15-SP7:Update".into(),
            },
            IncidentAction {
                source_project: "home:u:branches:SUSE:SLE-15-SP6:Update".into(),
                source_package: "curl".into(),
                release_project: "SUSE:SLE-15-SP6:Update".into(),
            },
        ],
        sourceupdate: Some("cleanup".into()),
        description: "- Update to 8.1 (bsc#1234567) & more <x>".into(),
    }
}

pub(super) fn job() -> backport::Job {
    backport::Job {
        patches: vec![backport::PreparedPatch {
            file_name: "CVE-2025-1234.patch".into(),
            bytes: b"--- a\n+++ b\n@@ -1 +1 @@\n".to_vec(),
            origin: "https://github.com/o/r/commit/abc1234.patch".into(),
            commit: Some("abc1234".into()),
            reason: "reject overlong header lines".into(),
        }],
        refs: vec!["bsc#1234567".into(), "CVE-2025-1234".into()],
    }
}

pub(super) struct Fixed(pub(super) Vec<Affected>);

impl StreamCheck for Fixed {
    fn check(&self, _: &str, _: &[String], _: &FixQuery<'_>) -> Vec<Affected> {
        self.0.clone()
    }
}
