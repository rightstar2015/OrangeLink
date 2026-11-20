//! Rules for the GitHub Actions workflows, checked by `cargo test` so a change
//! that undoes one fails locally too, not only in CI's zizmor run. Each rule
//! closes a hole found in the update security review:
//!
//! - every action is pinned to a commit SHA (a moved tag can't swap the code);
//! - checkout keeps no token in .git/config (`persist-credentials: false`), so
//!   the build scripts and tests of ~600 crates can't read it;
//! - `run:` scripts get `${{ … }}` values only through `env:`, never pasted in
//!   (a tag named `v1$(…)` would run as a command);
//! - in release.yml only the job that creates the draft may write, the tag is
//!   checked against `vX.Y.Z`, and an existing release is never overwritten;
//! - Rust comes from rust-toolchain.toml, not whatever `stable` is that day;
//! - the README in the zip starts with a pointer to the project page.

/// The sentence the zip's README starts with, and the page it links to.
const ZIP_README_NOTE: &str = "完整說明與圖片請見 GitHub 專案頁面";
const PROJECT_PAGE: &str = "https://github.com/rightstar2015/OrangeLink";

/// The indentation of a YAML line.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// What breaks the rules in a workflow, one line per problem.
fn problems(name: &str, yml: &str) -> Vec<String> {
    // A checkout with core.autocrlf=true has CRLF line ends.
    let yml = yml.replace("\r\n", "\n");
    let lines: Vec<&str> = yml.lines().collect();
    let mut out = Vec::new();
    let mut in_run: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.contains("rustup update") || t.contains("dtolnay/rust-toolchain") {
            out.push(format!("{name}:{n}: Rust not taken from rust-toolchain.toml"));
        }
        if t.contains("--clobber") {
            out.push(format!("{name}:{n}: --clobber would replace a release's files"));
        }
        if let Some(run_indent) = in_run {
            if indent(line) > run_indent {
                if line.contains("${{") {
                    out.push(format!("{name}:{n}: ${{{{ }}}} inside a run script; pass it through env:"));
                }
                continue;
            }
            in_run = None;
        }
        let key = t.trim_start_matches("- ");
        if let Some(script) = key.strip_prefix("run:") {
            if script.contains("${{") {
                out.push(format!("{name}:{n}: ${{{{ }}}} inside a run script; pass it through env:"));
            }
            in_run = Some(indent(line) + usize::from(t.starts_with("- ")) * 2);
        }
        if let Some(action) = key.strip_prefix("uses:") {
            let action = action.split('#').next().unwrap_or_default().trim();
            let pinned = action.split_once('@').is_some_and(|(_, r)| r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()));
            if !pinned {
                out.push(format!("{name}:{n}: {action} is not pinned to a commit SHA"));
            }
            if action.starts_with("actions/checkout@") {
                let rest = lines[i + 1..].iter().take_while(|l| l.trim().is_empty() || indent(l) > indent(line));
                if !rest.into_iter().any(|l| l.trim() == "persist-credentials: false") {
                    out.push(format!("{name}:{n}: checkout without persist-credentials: false"));
                }
            }
        }
    }
    out
}

/// release.yml's own rules: who may write, the tag check, no overwriting.
fn release_problems(yml: &str) -> Vec<String> {
    // A checkout with core.autocrlf=true has CRLF line ends, and the job
    // blocks below are found by splitting at "\njobs:\n".
    let yml = yml.replace("\r\n", "\n");
    let yml = yml.as_str();
    let mut out = Vec::new();
    // Each job's block: from `  <name>:` at indent 2 under `jobs:` to the next.
    let jobs = yml.split_once("\njobs:\n").map(|(_, j)| j).unwrap_or_default();
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in jobs.lines() {
        if indent(line) == 2 && line.trim_end().ends_with(':') && !line.trim().starts_with('#') {
            blocks.push((line.trim().trim_end_matches(':').to_string(), String::new()));
        } else if let Some((_, body)) = blocks.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let top = yml.split_once("\njobs:\n").map(|(t, _)| t).unwrap_or(yml);
    if !top.lines().any(|l| l.trim() == "permissions: {}") {
        out.push("release.yml: the workflow's default permissions are not empty (`permissions: {}`)".into());
    }
    for (job, body) in &blocks {
        let builds = body.contains("cargo ");
        // A job without its own permissions gets the workflow's.
        let writes = body.contains("contents: write") || (!body.contains("permissions:") && top.contains("contents: write"));
        if !body.contains("permissions:") {
            out.push(format!("release.yml: job {job} does not set its own permissions"));
        }
        if builds && writes {
            out.push(format!("release.yml: job {job} builds and can write to the repository"));
        }
        if writes && !body.contains("gh release view") {
            out.push(format!("release.yml: job {job} writes without first checking for an existing release"));
        }
        if writes {
            // Exactly the zip, SHA256SUMS.txt and latest.json are published,
            // checked before the release is made and named one by one.
            let check = body.find(r#"expected=$(printf '%s\n' "OrangeLink-$TAG-windows-x64.zip" SHA256SUMS.txt latest.json | LC_ALL=C sort)"#);
            let compare = body.find(r#"if [ "$expected" != "$actual" ]; then"#);
            let create = body.lines().find(|l| l.contains("gh release create") || l.contains("gh release upload"));
            let create_at = create.and_then(|l| body.find(l));
            let checked_first = matches!((check, compare, create_at), (Some(a), Some(b), Some(c)) if a < b && b < c);
            if !checked_first || !body.contains("find . -mindepth 1") {
                out.push(format!("release.yml: job {job} does not check it publishes exactly the zip, SHA256SUMS.txt and latest.json"));
            }
            if create.is_some_and(|l| l.contains('*')) {
                out.push(format!("release.yml: job {job} publishes files picked by a wildcard"));
            }
            // The files agree with each other before they are published.
            let sums = body.find("(cd dist && sha256sum --check --strict SHA256SUMS.txt)");
            let fields = body.find(r#"'.asset == $asset and .sha256 == $sha and .version == $version' dist/latest.json"#);
            let hashed = body.contains(r#"hash=$(sha256sum "dist/$zip""#) && body.contains(r#"--arg sha "$hash""#);
            if !matches!((sums, create_at), (Some(a), Some(c)) if a < c) {
                out.push(format!("release.yml: job {job} does not check the zip against SHA256SUMS.txt before publishing"));
            }
            if !hashed || !matches!((fields, create_at), (Some(a), Some(c)) if a < c) {
                out.push(format!("release.yml: job {job} does not check latest.json's asset, sha256 and version before publishing"));
            }
        }
        if body.contains("ref_name") && !body.contains("TAG: ${{ github.ref_name }}") {
            out.push(format!("release.yml: job {job} reads the tag other than through env TAG"));
        }
        if body.contains("$TAG") || body.contains("$env:TAG") {
            let checked = body.contains(r"'^v[0-9]+\.[0-9]+\.[0-9]+$'") || body.contains(r"^v[0-9]+\.[0-9]+\.[0-9]+$ ]]");
            if !checked {
                out.push(format!("release.yml: job {job} uses the tag without checking it is vX.Y.Z"));
            }
        }
        // The README in the zip points to the project page, because the images it
        // shows (docs/images) are not packaged.
        if body.contains("README.md") && !(body.contains(ZIP_README_NOTE) && body.contains(PROJECT_PAGE)) {
            out.push(format!("release.yml: job {job} packages the README without the pointer to the project page"));
        }
    }
    if !blocks.iter().any(|(_, b)| b.contains("contents: write")) {
        out.push("release.yml: no job can create the draft".into());
    }
    out
}

/// rust-toolchain.toml names an exact version (`1.99.1`), not a channel.
fn toolchain_problems(toml: &str) -> Vec<String> {
    let toml = toml.replace("\r\n", "\n");
    let channel = toml.lines().find_map(|l| l.trim().strip_prefix("channel")).map(|v| v.trim_start_matches([' ', '=']).trim().trim_matches('"'));
    let exact = channel.is_some_and(|v| v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())));
    if exact {
        Vec::new()
    } else {
        vec![format!("rust-toolchain.toml: channel {channel:?} is not an exact version")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CI: &str = include_str!("../.github/workflows/ci.yml");
    const RELEASE: &str = include_str!("../.github/workflows/release.yml");
    const TOOLCHAIN: &str = include_str!("../rust-toolchain.toml");

    #[test]
    fn the_workflows_keep_their_security_rules() {
        let mut all = problems("ci.yml", CI);
        all.extend(problems("release.yml", RELEASE));
        all.extend(release_problems(RELEASE));
        all.extend(toolchain_problems(TOOLCHAIN));
        assert!(all.is_empty(), "\n{}", all.join("\n"));
    }

    /// LF or CRLF, as a checkout with core.autocrlf=false or true has them.
    fn with_line_ends(text: &str, crlf: bool) -> String {
        let lf = text.replace("\r\n", "\n");
        if crlf { lf.replace('\n', "\r\n") } else { lf }
    }

    #[test]
    fn the_workflow_rules_hold_for_a_crlf_checkout() {
        for crlf in [false, true] {
            let (ci, release, toolchain) = (with_line_ends(CI, crlf), with_line_ends(RELEASE, crlf), with_line_ends(TOOLCHAIN, crlf));
            let mut all = problems("ci.yml", &ci);
            all.extend(problems("release.yml", &release));
            all.extend(release_problems(&release));
            all.extend(toolchain_problems(&toolchain));
            assert!(all.is_empty(), "crlf={crlf}:\n{}", all.join("\n"));
            // The job rules really ran: a workflow that breaks one is caught.
            let broken = with_line_ends(&release.replace("\r\n", "\n").replacen("      contents: read\n", "      contents: write\n", 1), crlf);
            assert!(release_problems(&broken).join("\n").contains("builds and can write"), "crlf={crlf}");
        }
    }

    #[test]
    fn the_publish_job_uploads_exactly_the_expected_files() {
        let release = RELEASE.replace("\r\n", "\n");
        assert!(release_problems(&release).is_empty());
        let rule = "exactly the zip, SHA256SUMS.txt and latest.json";
        // No check at all.
        let unchecked = release.replace(r#"if [ "$expected" != "$actual" ]; then"#, "if false; then");
        assert!(release_problems(&unchecked).join("\n").contains(rule));
        // A fourth file allowed.
        let extra = release.replace(r#""OrangeLink-$TAG-windows-x64.zip" SHA256SUMS.txt latest.json"#, r#""OrangeLink-$TAG-windows-x64.zip" SHA256SUMS.txt latest.json extra.exe"#);
        assert!(release_problems(&extra).join("\n").contains(rule));
        // Files picked by a wildcard.
        let wild = release.replace(r#""dist/OrangeLink-$TAG-windows-x64.zip""#, "dist/*.zip");
        assert!(release_problems(&wild).join("\n").contains("wildcard"));
        // The check after the release is made is too late.
        let start = release.find("          # Exactly the three files").unwrap();
        let end = release.find("          gh release create").unwrap();
        let late = format!("{}{}{}", &release[..start], &release[end..], &release[start..end]);
        assert!(release_problems(&late).join("\n").contains(rule));
    }

    #[test]
    fn the_publish_job_checks_the_files_agree() {
        let release = RELEASE.replace("\r\n", "\n");
        assert!(release_problems(&release).is_empty());
        let no_sums = release.replace("(cd dist && sha256sum --check --strict SHA256SUMS.txt)", "true");
        assert!(release_problems(&no_sums).join("\n").contains("SHA256SUMS.txt before publishing"));
        let no_jq = release.replace(".asset == $asset and .sha256 == $sha and .version == $version", ".asset == $asset");
        assert!(release_problems(&no_jq).join("\n").contains("asset, sha256 and version"));
        let unhashed = release.replace(r#"--arg sha "$hash""#, r#"--arg sha "$(jq -r .sha256 dist/latest.json)""#);
        assert!(release_problems(&unhashed).join("\n").contains("asset, sha256 and version"), "the hash must be the file's own");
        // A check after the release is made is too late.
        let start = release.find("          # The three agree with each other").unwrap();
        let end = release.find("          gh release create").unwrap();
        let late = format!("{}{}{}", &release[..start], &release[end..], &release[start..end]);
        assert!(release_problems(&late).join("\n").contains("before publishing"));
    }

    /// The licence a crate registry or `cargo metadata` shows is the one in LICENSE.
    #[test]
    fn the_cargo_license_matches_the_license_file() {
        let cargo = include_str!("../Cargo.toml");
        let license = include_str!("../LICENSE");
        assert!(cargo.lines().any(|l| l.trim_end() == "license = \"MIT\""), "Cargo.toml names no MIT licence");
        assert!(license.starts_with("MIT License"), "LICENSE is not the MIT licence");
    }

    #[test]
    fn the_zip_readme_points_to_the_project_page() {
        let release = RELEASE.replace("\r\n", "\n");
        assert!(release_problems(&release).is_empty());
        let rule = "without the pointer to the project page";
        // Back to copying the README as it is.
        let plain = release.replace(ZIP_README_NOTE, "");
        assert!(release_problems(&plain).join("\n").contains(rule));
        // A pointer that leads somewhere else.
        let elsewhere = release.replace(PROJECT_PAGE, "https://example.com");
        assert!(release_problems(&elsewhere).join("\n").contains(rule));
        // The note is really written in front of the README's own text.
        assert!(release.contains(r#"$note + [IO.File]::ReadAllText("$PWD/README.md")"#));
    }

    #[test]
    fn the_workflow_rules_catch_each_regression() {
        // Each of these was in release.yml before the security review.
        let old = r#"name: Release
on:
  push:
    tags: ["v*"]
permissions:
  contents: write
jobs:
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build and test
        run: |
          rustup update stable --no-self-update
          cargo build --release --locked
      - name: Publish
        shell: pwsh
        run: |
          $tag = "${{ github.ref_name }}"
          gh release upload $tag dist/latest.json --clobber
"#;
        let p = problems("old.yml", old).join("\n");
        for want in ["not pinned", "persist-credentials", "inside a run script", "rust-toolchain.toml", "--clobber"] {
            assert!(p.contains(want), "missed {want}:\n{p}");
        }
        let r = release_problems(old).join("\n");
        for want in ["permissions are not empty", "builds and can write", "other than through env TAG", "exactly the zip"] {
            assert!(r.contains(want), "missed {want}:\n{r}");
        }
        // A one-line run script, and a pinned action with a checkout that keeps the token.
        let p = problems("x.yml", "    steps:\n      - run: echo ${{ github.head_ref }}\n      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4\n        with:\n          fetch-depth: 0\n").join("\n");
        assert!(p.contains("inside a run script") && p.contains("persist-credentials") && !p.contains("not pinned"), "{p}");
        // `${{ }}` in env: is fine.
        assert!(problems("x.yml", "    env:\n      TAG: ${{ github.ref_name }}\n    steps:\n      - run: echo \"$TAG\"\n").is_empty());
        assert!(!toolchain_problems("[toolchain]\nchannel = \"stable\"\n").is_empty());
        assert!(toolchain_problems("[toolchain]\nchannel = \"1.99.1\"\n").is_empty());
    }
}
