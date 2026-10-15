// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Advisory checks over an `autounattend.xml`. Every rule here is a failure this
//! project has already paid for. With no VNC on the systems in the rack, this
//! protects users from a dead vm that will not respond.
//!
//! As prose that list protects nothing automatically. As a linter it runs against a
//! user's supplied file *and* against our own generated output
//! (`generated_output_is_clean`), so a future change to the generator that
//! reintroduces one of these fails the suite rather than a rack.
//!
//! Advisory, not a gate: a malformed file is refused because Setup would refuse it
//! anyway, and everything else warns and builds.

use crate::settings::Problem;
use crate::unattend::PATH_LIMIT;

/// What the surrounding build expects of the answer file, so the rules that depend
/// on context can fire.
///
/// Two fields rather than a `&Config`, because the file being linted may not have
/// come from a `Config` at all — that is the entire point of supplying one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintContext {
    /// The disk the build is targeting. An answer file naming another one installs
    /// Windows onto our own installer media, or onto nothing.
    pub target_disk: u8,
    /// Whether this build syspreps. Changes which passes run, and therefore which
    /// components have to be present.
    pub generalize: bool,
    /// Whether this build carries cloud-init. It is installed by
    /// `bootstrap.ps1`, so an answer file that never invokes the bootstrap
    /// produces a clone where every cloud-init command fails.
    pub cloud_init: bool,
}

/// Every rule is scanned rather than parsed, matching `wim::parse_xml`: the shape is
/// fixed and flat, and a real XML dependency would buy a second set of failure modes
/// for no benefit. The one structural check is the parse gate in
/// [`looks_like_unattend_xml`].
pub fn lint(xml: &str, cx: &LintContext) -> Vec<Problem> {
    let mut v = Vec::new();

    if !looks_like_unattend_xml(xml) {
        v.push(Problem::block(
            "unattend",
            "This does not look like an unattend answer file: no <unattend> \
             element. Setup would reject it.",
        ));
        // Every rule below reads structure this file does not have, so they would
        // each fire and bury the one finding that matters.
        return v;
    }

    // <Path> is capped at 259 by the schema. Over it, Setup rejects the whole answer
    // file with an error naming only the pass, which reads like malformed XML.
    for path in elements(xml, "Path") {
        if path.len() > PATH_LIMIT {
            v.push(Problem::warn(
                "unattend_path",
                format!(
                    "A <Path> is {} characters; the schema caps them at \
                     {PATH_LIMIT}. Setup rejects the whole answer file, with an \
                     error naming only the pass.",
                    path.len()
                ),
            ));
            break;
        }
    }

    // An empty <Key> is not the same as omitting <ProductKey>: empty means "install
    // with no key", Setup resolves it against no edition, and the install stalls at
    // the license terms before showing a page.
    if elements(xml, "Key").iter().any(|k| k.trim().is_empty()) {
        v.push(Problem::warn(
            "unattend_product_key",
            "<ProductKey> has an empty <Key>. That means \"install with no key\" \
             and stalls at the license terms. To use no key, remove the element.",
        ));
    }

    // Valid only in offlineServicing and auditSystem. In specialize Setup ignores
    // the whole component with no error, NetKVM is never installed, and the guest
    // comes up with no network.
    for (pass, body) in passes(xml) {
        let misplaced = body
            .contains("Microsoft-Windows-PnpCustomizationsNonWinPE")
            && !matches!(pass.as_str(), "offlineServicing" | "auditSystem");
        if misplaced {
            v.push(Problem::warn(
                "unattend_pnp_pass",
                format!(
                    "PnpCustomizationsNonWinPE is in the {pass} pass, where Setup \
                     ignores it with no error. The drivers never install and the \
                     guest comes up with no network. It belongs in \
                     offlineServicing."
                ),
            ));
        }
    }

    // The bootstrap is what installs the SSH keys, configures RDP and writes
    // C:\oxide-bootstrap.log. Without it the install still succeeds and the guest is
    // unreachable — this is the VOLUME_LABEL failure wearing a different hat.
    if !xml.contains("bootstrap.ps1") {
        v.push(Problem::warn(
            "unattend_bootstrap",
            "Nothing references setup\\bootstrap.ps1, so the guest bootstrap never \
             runs: no SSH keys, no RDP configuration and no \
             C:\\oxide-bootstrap.log. The install will still appear to succeed.",
        ));
    }

    if cx.cloud_init && !xml.contains("bootstrap.ps1") {
        // Only a golden build has a sysprep answer file to fail on the clones;
        // on a named build the cost is simply that nothing installs it.
        let consequence = if cx.generalize {
            "The generated sysprep answer file still runs it on each clone, so \
             every one of those commands will fail with nothing installed to \
             run."
        } else {
            "Nothing else installs it, so the machine comes up with no \
             cloud-init: no per-instance SSH keys and no user_data."
        };
        v.push(Problem::warn(
            "unattend_cloud_init",
            format!(
                "This file does not invoke setup\\bootstrap.ps1, which is what \
                 installs cloud-init. {consequence}"
            ),
        ));
    }

    // Everything in this workspace is amd64: the drivers are amd64 and nothing here
    // works on arm64. Every occurrence is read, not just the presence of one good
    // one: a file whose components are amd64 apart from a single arm64 one is the
    // interesting case, and asking only whether "amd64" appears anywhere reports it
    // clean.
    if let Some(other) = attributes(xml, "processorArchitecture")
        .iter()
        .find(|arch| arch != &"amd64")
    {
        v.push(Problem::warn(
            "unattend_arch",
            format!(
                "A component declares processorArchitecture={other:?}. The \
                 payload drivers are amd64 and nothing in this tool supports \
                 another architecture."
            ),
        ));
    }

    let passwords = elements(xml, "Value");
    if passwords.iter().any(|p| p.contains("*SENSITIVE*DATA*DELETED*")) {
        v.push(Problem::warn(
            "unattend_password",
            "The password is the literal *SENSITIVE*DATA*DELETED*. Windows writes \
             that into the copy it caches at C:\\Windows\\Panther\\unattend.xml, so \
             this file was taken from a guest. The account cannot be created and \
             OOBE will wait for a human forever.",
        ));
    } else if !xml.contains("<Password>") {
        v.push(Problem::warn(
            "unattend_password",
            "No <Password> anywhere. The local account is not created, and OOBE \
             stops at the out-of-box wizard — on a guest with no framebuffer, \
             forever.",
        ));
    }

    // On an Oxide guest there is no console to show UI on, so Setup waits for a
    // click nobody can make. From outside that is indistinguishable from a slow
    // install.
    if elements(xml, "WillShowUI").iter().any(|u| u.trim() == "OnError") {
        v.push(Problem::warn(
            "unattend_will_show_ui",
            "A <WillShowUI> is OnError. An Oxide guest has no framebuffer, so \
             Setup waits forever for a click nobody can make. Never fails fast \
             instead.",
        ));
    }

    // Any disk that is not the target, not merely "the target appears nowhere". A
    // file carrying both <DiskID>0</DiskID> and <DiskID>1</DiskID> is exactly the
    // case this message describes -- something in it addresses the installer media
    // -- and the weaker test reported it clean.
    let expected = cx.target_disk.to_string();
    if let Some(other) =
        elements(xml, "DiskID").iter().find(|id| id.trim() != expected)
    {
        v.push(Problem::warn(
            "unattend_disk",
            format!(
                "A <DiskID> is {}, but this build targets disk {expected}. \
                 Disk 0 is the installer media; installing onto it destroys the \
                 media mid-install.",
                other.trim()
            ),
        ));
    }

    // A generalize cycle never runs windowsPE, so nothing sets the locale, OOBE's
    // Localization page becomes active, and it stops there waiting for a click.
    // Caught on a rack: `SETACTIVE: wizard page for page Localization`.
    //
    // This only applies to the sysprep answer file itself — the one `build_sysprep`
    // emits, which has no `windowsPE` pass at all. A normal `autounattend.xml` for a
    // golden build (`cx.generalize` true, but a `windowsPE` pass present) sets the
    // locale from `Microsoft-Windows-International-Core-WinPE` in that pass instead,
    // and deliberately omits the non-WinPE component; firing here would tell a user
    // to add a component that belongs in a different document.
    let has_windows_pe =
        passes(xml).iter().any(|(pass, _)| pass == "windowsPE");
    if cx.generalize
        && !has_windows_pe
        && !xml.contains(r#"name="Microsoft-Windows-International-Core""#)
    {
        v.push(Problem::warn(
            "unattend_locale",
            "This is a golden build and no Microsoft-Windows-International-Core \
             component is present. A generalize cycle never runs windowsPE, so \
             nothing sets the locale, and OOBE stops on the Localization page \
             forever.",
        ));
    }

    v
}

/// The parse gate. Not a real parse: the question is only whether Setup would find
/// an answer file here at all, and everything downstream scans rather than parses.
fn looks_like_unattend_xml(xml: &str) -> bool {
    xml.contains("<unattend") && xml.contains("</unattend>")
}

/// Every `<TAG>...</TAG>` body in the document, in order. Unlike `wim::element`,
/// which wants the first, every rule here wants all of them.
fn elements(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let from = at + open.len();
        let Some(end) = rest[from..].find(&close) else { break };
        found.push(rest[from..from + end].to_string());
        rest = &rest[from + end + close.len()..];
    }
    found
}

/// Every `name="..."` attribute value in the document, in order.
///
/// The counterpart to [`elements`] for the rules that read an attribute rather than
/// an element body. Every occurrence, because a rule that asks only whether a good
/// value appears somewhere cannot see a bad one next to it.
fn attributes(xml: &str, name: &str) -> Vec<String> {
    let open = format!("{name}=\"");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let from = at + open.len();
        let Some(end) = rest[from..].find('"') else { break };
        found.push(rest[from..from + end].to_string());
        rest = &rest[from + end + 1..];
    }
    found
}

/// Each `<settings pass="…">` block as (pass name, body), so a rule can ask which
/// pass something is in. That question is the whole of the PnP trap.
fn passes(xml: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(r#"<settings pass=""#) {
        let from = at + r#"<settings pass=""#.len();
        let Some(quote) = rest[from..].find('"') else { break };
        let name = rest[from..from + quote].to_string();
        let body_start = from + quote;
        let end = rest[body_start..]
            .find("</settings>")
            .map(|e| body_start + e)
            .unwrap_or(rest.len());
        found.push((name, rest[body_start..end].to_string()));
        rest = &rest[end..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> LintContext {
        LintContext { target_disk: 1, generalize: false, cloud_init: false }
    }

    fn fields(problems: &[Problem]) -> Vec<&str> {
        problems.iter().map(|p| p.field).collect()
    }

    /// The minimum shape a rule needs to *not* fire. Each test below perturbs one
    /// thing, so anything that fires on this baseline is a false positive.
    fn clean() -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64">
      <DiskConfiguration>
        <Disk wcm:action="add"><DiskID>1</DiskID></Disk>
      </DiskConfiguration>
    </component>
  </settings>
  <settings pass="offlineServicing">
    <component name="Microsoft-Windows-PnpCustomizationsNonWinPE" processorArchitecture="amd64">
      <DriverPaths><PathAndCredentials><Path>C:\drivers</Path></PathAndCredentials></DriverPaths>
    </component>
  </settings>
  <settings pass="specialize">
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add">
          <Path>powershell -File setup\bootstrap.ps1</Path>
        </RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64">
      <UserAccounts><LocalAccounts><LocalAccount wcm:action="add">
        <Password><Value>{p}</Value><PlainText>true</PlainText></Password>
      </LocalAccount></LocalAccounts></UserAccounts>
    </component>
  </settings>
</unattend>
"#,
            p = "0xide!230xide!23"
        )
    }

    #[test]
    fn a_clean_file_reports_nothing() {
        let found = lint(&clean(), &cx());
        assert!(found.is_empty(), "false positives: {found:?}");
    }

    #[test]
    fn unparseable_xml_blocks() {
        let found = lint("not xml at all", &cx());
        assert!(found.iter().any(|p| p.field == "unattend" && p.blocking));
    }

    #[test]
    fn an_overlong_path_warns() {
        // 260 characters: one over PATH_LIMIT. Setup rejects the whole answer file
        // with a message naming only the pass, which reads like malformed XML.
        let long = "a".repeat(260);
        let xml = clean().replace(r"C:\drivers", &long);
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_path"));
    }

    #[test]
    fn a_path_at_the_limit_is_fine() {
        let at = "a".repeat(259);
        let xml = clean().replace(r"C:\drivers", &at);
        assert!(!fields(&lint(&xml, &cx())).contains(&"unattend_path"));
    }

    #[test]
    fn an_empty_product_key_warns() {
        // Empty means "install with no key" and stalls at the EULA. Omitting the
        // element entirely is the correct way to say "no key".
        let xml = clean().replace(
            "<DiskConfiguration>",
            "<UserData><ProductKey><Key></Key></ProductKey></UserData><DiskConfiguration>",
        );
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_product_key"));
    }

    #[test]
    fn pnp_customizations_in_specialize_warns() {
        // Valid only in offlineServicing/auditSystem. In specialize it is ignored
        // with no error and NetKVM never installs, so the guest has no network.
        let xml = clean().replace(
            r#"<settings pass="specialize">"#,
            r#"<settings pass="specialize">
    <component name="Microsoft-Windows-PnpCustomizationsNonWinPE" processorArchitecture="amd64"/>"#,
        );
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_pnp_pass"));
    }

    #[test]
    fn a_missing_bootstrap_reference_warns() {
        let xml = clean().replace(r"setup\bootstrap.ps1", "notepad.exe");
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_bootstrap"));
    }

    #[test]
    fn a_non_amd64_architecture_warns() {
        let xml = clean().replace("amd64", "arm64");
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_arch"));
    }

    /// The case the rule exists for, and the one the whole-file replacement above
    /// cannot distinguish: three amd64 components and one arm64 one. Asking only
    /// whether "amd64" appears anywhere reports this clean, and the component that
    /// would never install is the one nobody looked at.
    #[test]
    fn one_arm64_component_among_amd64_ones_warns() {
        let xml = clean().replacen(
            r#"processorArchitecture="amd64""#,
            r#"processorArchitecture="arm64""#,
            1,
        );
        let found = lint(&xml, &cx());
        assert!(fields(&found).contains(&"unattend_arch"));
        assert!(
            found.iter().any(|p| p.message.contains("arm64")),
            "the finding should name the architecture it found: {found:?}"
        );
    }

    /// And the converse: every component amd64 is silent, so the rule reads the
    /// values rather than merely counting them.
    #[test]
    fn every_component_amd64_is_silent() {
        assert!(!fields(&lint(&clean(), &cx())).contains(&"unattend_arch"));
    }

    #[test]
    fn a_missing_password_warns() {
        let xml = clean().replace("<Password><Value>0xide!230xide!23</Value><PlainText>true</PlainText></Password>", "");
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_password"));
    }

    #[test]
    fn a_scrubbed_password_warns() {
        // Windows replaces every <Password> with this literal in the copy it caches
        // at C:\Windows\Panther\unattend.xml. Someone who copied that file out has
        // an answer file that cannot create the account, and OOBE stops forever.
        let xml =
            clean().replace("0xide!230xide!23", "*SENSITIVE*DATA*DELETED*");
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_password"));
    }

    #[test]
    fn will_show_ui_on_error_warns() {
        let xml = clean()
            .replace("<DiskConfiguration>", "<WillShowUI>OnError</WillShowUI>");
        assert!(fields(&lint(&xml, &cx())).contains(&"unattend_will_show_ui"));
    }

    #[test]
    fn a_disk_id_disagreeing_with_the_target_warns() {
        let cx = LintContext {
            target_disk: 2,
            generalize: false,
            cloud_init: false,
        };
        assert!(fields(&lint(&clean(), &cx)).contains(&"unattend_disk"));
    }

    /// The dangerous shape the message describes, and the one the earlier rule
    /// called clean: the target disk is named, so "at least one DiskID matches"
    /// was satisfied, and disk 0 -- the installer media -- was named as well.
    #[test]
    fn a_file_naming_both_the_target_and_disk_zero_warns() {
        let xml = clean().replace(
            "<Disk wcm:action=\"add\"><DiskID>1</DiskID></Disk>",
            "<Disk wcm:action=\"add\"><DiskID>1</DiskID></Disk>\n        \
             <Disk wcm:action=\"add\"><DiskID>0</DiskID></Disk>",
        );
        let found = lint(&xml, &cx());
        assert!(fields(&found).contains(&"unattend_disk"));
        assert!(
            found.iter().any(|p| p.message.contains("A <DiskID> is 0")),
            "the finding should name the disk it found: {found:?}"
        );
    }

    /// A file naming only the target disk, however many times, is silent.
    #[test]
    fn every_disk_id_matching_the_target_is_silent() {
        let xml = clean().replace(
            "<Disk wcm:action=\"add\"><DiskID>1</DiskID></Disk>",
            "<Disk wcm:action=\"add\"><DiskID>1</DiskID></Disk>\n        \
             <InstallTo><DiskID>1</DiskID><PartitionID>3</PartitionID></InstallTo>",
        );
        assert!(!fields(&lint(&xml, &cx())).contains(&"unattend_disk"));
    }

    #[test]
    fn a_golden_build_without_international_core_warns() {
        // The sysprep answer file — the one `build_sysprep` emits — has no
        // windowsPE pass at all, which is the shape this rule actually guards:
        // nothing else in the document sets the locale.
        let cx =
            LintContext { target_disk: 1, generalize: true, cloud_init: false };
        let xml = clean().replace(
            r#"  <settings pass="windowsPE">
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64">
      <DiskConfiguration>
        <Disk wcm:action="add"><DiskID>1</DiskID></Disk>
      </DiskConfiguration>
    </component>
  </settings>
"#,
            "",
        );
        assert!(fields(&lint(&xml, &cx)).contains(&"unattend_locale"));
    }

    /// A normal `autounattend.xml` for a golden build: `windowsPE` is present (it
    /// sets the locale through `International-Core-WinPE`, which this fixture
    /// does not model) and the non-WinPE `International-Core` component is
    /// deliberately absent, per `oobe_pass`'s doc comment. This must not warn —
    /// doing so would tell a user to add a component that belongs in a different
    /// document (the sysprep file), not this one.
    #[test]
    fn a_golden_build_with_a_windows_pe_pass_is_fine_without_international_core()
     {
        let cx =
            LintContext { target_disk: 1, generalize: true, cloud_init: false };
        assert!(!fields(&lint(&clean(), &cx)).contains(&"unattend_locale"));
    }

    #[test]
    fn a_golden_build_with_international_core_is_fine() {
        let cx =
            LintContext { target_disk: 1, generalize: true, cloud_init: false };
        let xml = clean().replace(
            r#"<settings pass="oobeSystem">"#,
            r#"<settings pass="oobeSystem">
    <component name="Microsoft-Windows-International-Core" processorArchitecture="amd64"/>"#,
        );
        assert!(!fields(&lint(&xml, &cx)).contains(&"unattend_locale"));
    }

    /// Nothing here is blocking except a parse failure. The user edited the file on
    /// purpose; the tool's job is to say what looks wrong, not to refuse.
    #[test]
    fn only_a_parse_failure_blocks() {
        let xml = clean().replace("amd64", "arm64").replace("1", "9");
        let found = lint(&xml, &cx());
        assert!(!found.is_empty(), "expected findings on a mangled file");
        assert!(
            found.iter().all(|p| !p.blocking),
            "a non-parse finding blocked: {found:?}"
        );
    }

    /// `build_sysprep` is ours either way, so the clone-side specialize run
    /// survives a supplied answer file. But if that file never invokes
    /// `setup\bootstrap.ps1`, the MSI is never installed, and every one of
    /// those commands fails on a clone -- silently. Same family as the existing
    /// bootstrap warning, and it belongs beside it.
    #[test]
    fn a_supplied_file_with_no_bootstrap_warns_about_cloud_init_too() {
        let cx = LintContext { cloud_init: true, ..cx() };
        let problems = lint("<unattend></unattend>", &cx);
        assert!(
            fields(&problems).contains(&"unattend_cloud_init"),
            "{problems:?}"
        );
        assert!(problems.iter().all(|p| !p.blocking || p.field == "unattend"));
    }

    /// A named build has no sysprep answer file, so its warning must not
    /// promise one: it names what does fail instead.
    #[test]
    fn the_cloud_init_warning_matches_the_kind_of_build() {
        let message = |generalize| {
            let cx = LintContext { cloud_init: true, generalize, ..cx() };
            lint("<unattend></unattend>", &cx)
                .into_iter()
                .find(|p| p.field == "unattend_cloud_init")
                .expect("the warning")
                .message
        };
        let named = message(false);
        assert!(!named.contains("sysprep"), "{named}");
        assert!(named.contains("no per-instance SSH keys"), "{named}");
        let golden = message(true);
        assert!(golden.contains("sysprep answer file"), "{golden}");
    }

    #[test]
    fn a_file_that_does_invoke_the_bootstrap_says_nothing_about_cloud_init() {
        let cx = LintContext { cloud_init: true, ..cx() };
        let xml = clean();
        assert!(
            !fields(&lint(&xml, &cx)).contains(&"unattend_cloud_init"),
            "{:?}",
            lint(&xml, &cx)
        );
    }

    #[test]
    fn with_cloud_init_off_the_rule_is_silent() {
        let cx = LintContext { cloud_init: false, ..cx() };
        assert!(
            !fields(&lint("<unattend></unattend>", &cx))
                .contains(&"unattend_cloud_init")
        );
    }
}
