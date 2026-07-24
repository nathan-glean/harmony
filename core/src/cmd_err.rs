//! Shared classification of external-command failures (`gh`, `git`, `acli`, `claude`) into
//! actionable, user-facing messages. The Tauri layer stringifies these straight into a toast,
//! so the wording is what the user reads — distinguish "not installed", "not authenticated",
//! and "network unreachable" from a generic failure rather than dumping raw stderr.

use std::io;

/// A command that couldn't even be spawned (the binary is missing, or the OS refused).
/// `ErrorKind::NotFound` is the common case: the CLI isn't installed / not on PATH.
pub fn spawn_error(tool: &str, e: &io::Error) -> anyhow::Error {
    if e.kind() == io::ErrorKind::NotFound {
        anyhow::anyhow!("{tool} is not installed or not on your PATH")
    } else {
        anyhow::anyhow!("could not run {tool}: {e}")
    }
}

/// Classify a command that ran but exited non-zero, from its stderr. Recognises auth and
/// network failures; otherwise passes the trimmed stderr through with the tool name.
///
/// The returned string is logged (`~/.harmony/harmony.log`) and shown in a toast, so any
/// stderr echoed through it is run through [`redact_secrets`] first — harmony holds no
/// tokens itself, but a user's credential-embedded git remote (or a future tool) can print
/// one on failure, and it must never reach a log line.
pub fn classify(tool: &str, stderr: &str) -> anyhow::Error {
    let s = stderr.trim();
    let low = s.to_lowercase();
    if is_auth_failure(&low) {
        anyhow::anyhow!("{tool} is not authenticated — {}", auth_hint(tool))
    } else if is_network_failure(&low) {
        let red = redact_secrets(s);
        anyhow::anyhow!(
            "network error reaching {tool} — check your connection{}",
            first_line(&red)
                .map(|l| format!(" ({l})"))
                .unwrap_or_default()
        )
    } else if s.is_empty() {
        anyhow::anyhow!("{tool} failed (no error output)")
    } else {
        anyhow::anyhow!("{tool} failed: {}", redact_secrets(s))
    }
}

/// Known token prefixes worth scrubbing wherever they appear (GitHub PATs/OAuth tokens and
/// Atlassian API tokens — the only credential shapes near harmony's tooling). A prefix match
/// plus a run of token characters is replaced with `<redacted>`.
const TOKEN_PREFIXES: [&str; 7] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "ATATT",
];

/// Strip credentials from a string before it is logged or shown to the user. Handles the two
/// realistic shapes: `scheme://user:token@host` userinfo in a URL, and bare tokens with a
/// recognisable prefix. Defensive — harmony never constructs these itself.
pub fn redact_secrets(s: &str) -> String {
    redact_tokens(redact_url_userinfo(s))
}

/// Replace the `user:pass@` (or `token@`) userinfo of any `scheme://…@host` URL with `***@`.
fn redact_url_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("://") {
            out.push_str("://");
            let after = &s[i + 3..];
            // Userinfo runs up to '@', but only within the authority — stop at a path/query
            // delimiter or whitespace, which means there was no userinfo.
            let mut at = None;
            for (j, c) in after.char_indices() {
                if c == '@' {
                    at = Some(j);
                    break;
                }
                if matches!(c, '/' | '?' | '#') || c.is_whitespace() {
                    break;
                }
            }
            if let Some(j) = at {
                out.push_str("***@");
                i += 3 + j + 1;
                continue;
            }
            i += 3;
            continue;
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Replace any prefixed token (see [`TOKEN_PREFIXES`]) with `<redacted>`.
fn redact_tokens(mut s: String) -> String {
    for prefix in TOKEN_PREFIXES {
        let mut from = 0;
        while let Some(rel) = s[from..].find(prefix) {
            let start = from + rel;
            let mut end = start + prefix.len();
            while let Some(c) = s[end..].chars().next() {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            // Only scrub something that actually looks like a token, not the bare prefix.
            if end - start >= prefix.len() + 8 {
                s.replace_range(start..end, "<redacted>");
                from = start + "<redacted>".len();
            } else {
                from = end.max(start + 1);
            }
        }
    }
    s
}

pub fn is_auth_failure(low: &str) -> bool {
    const PATTERNS: [&str; 8] = [
        "not logged in",
        "not logged into",
        "authentication",
        "unauthorized",
        "auth login",
        "gh auth login",
        "401",
        "credentials",
    ];
    PATTERNS.iter().any(|p| low.contains(p))
}

pub fn is_network_failure(low: &str) -> bool {
    const PATTERNS: [&str; 10] = [
        "could not resolve host",
        "couldn't resolve host",
        "network is unreachable",
        "connection refused",
        "connection timed out",
        "could not connect",
        "failed to connect",
        "no such host",
        "temporary failure in name resolution",
        "dial tcp",
    ];
    PATTERNS.iter().any(|p| low.contains(p))
}

fn auth_hint(tool: &str) -> &'static str {
    match tool {
        "gh" => "run `gh auth login`",
        "acli" => "run `acli jira auth login`",
        _ => "check your credentials",
    }
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_not_found_says_not_installed() {
        let e = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");
        let msg = spawn_error("gh", &e).to_string();
        assert!(msg.contains("gh"));
        assert!(msg.contains("not installed"));
    }

    #[test]
    fn classifies_auth_failures() {
        // gh's typical logged-out message, and a generic acli auth-expired one.
        let gh = classify(
            "gh",
            "gh: To get started with GitHub CLI, please run: gh auth login",
        )
        .to_string();
        assert!(gh.contains("not authenticated"));
        assert!(gh.contains("gh auth login"));
        let acli = classify("acli", "Error: you are not logged in").to_string();
        assert!(acli.contains("not authenticated"));
        assert!(acli.contains("acli jira auth login"));
    }

    #[test]
    fn classifies_network_failures() {
        for stderr in [
            "fatal: unable to access 'https://github.com/x.git/': Could not resolve host: github.com",
            "dial tcp: lookup api.atlassian.com: no such host",
            "ssh: connect to host github.com port 22: Connection refused",
        ] {
            let msg = classify("git", stderr).to_string();
            assert!(msg.contains("network error"), "expected network classification for: {stderr}");
        }
    }

    #[test]
    fn falls_back_to_raw_stderr() {
        let msg = classify("git", "fatal: not a git repository").to_string();
        assert!(msg.contains("not a git repository"));
        // Empty stderr still produces a sensible message rather than a bare "failed:".
        assert!(classify("gh", "   ")
            .to_string()
            .contains("no error output"));
    }

    #[test]
    fn redacts_credential_url_userinfo() {
        let s = "fatal: unable to access 'https://x-access-token:ghp_ABCDEFGHIJ1234567890@github.com/o/r.git/': 403";
        let out = redact_secrets(s);
        assert!(
            !out.contains("ghp_ABCDEFGHIJ1234567890"),
            "token leaked: {out}"
        );
        assert!(!out.contains("x-access-token"), "userinfo leaked: {out}");
        assert!(
            out.contains("https://***@github.com/o/r.git"),
            "url mangled: {out}"
        );
    }

    #[test]
    fn redacts_bare_prefixed_tokens() {
        let out = redact_secrets("remote said: token ghp_ABCDEFGHIJ1234567890 rejected");
        assert!(out.contains("<redacted>"), "not redacted: {out}");
        assert!(
            !out.contains("ghp_ABCDEFGHIJ1234567890"),
            "token leaked: {out}"
        );
        // Atlassian token shape too.
        let out2 = redact_secrets("Authorization ATATT3xFfGF0abcdEFGHijk failed");
        assert!(
            !out2.contains("ATATT3xFfGF0abcdEFGHijk"),
            "atlassian token leaked: {out2}"
        );
    }

    #[test]
    fn classify_redacts_before_surfacing() {
        // A credential-embedded git remote echoed on a generic (non-auth, non-network) failure
        // must not reach the returned (logged + toasted) string.
        let msg = classify(
            "git",
            "fatal: could not read Password for 'https://user:ghp_ABCDEFGHIJ1234567890@github.com': terminal prompts disabled",
        )
        .to_string();
        assert!(
            !msg.contains("ghp_ABCDEFGHIJ1234567890"),
            "token leaked into message: {msg}"
        );
    }

    #[test]
    fn redaction_leaves_ordinary_text_and_ssh_untouched() {
        // No scheme:// userinfo, no token prefix → unchanged.
        assert_eq!(
            redact_secrets("fatal: not a git repository"),
            "fatal: not a git repository"
        );
        // SSH-style git@host has no scheme, and the host isn't a secret.
        assert_eq!(
            redact_secrets("git@github.com: Permission denied"),
            "git@github.com: Permission denied"
        );
        // A plain URL with no userinfo is preserved verbatim.
        assert_eq!(
            redact_secrets("cloning https://github.com/o/r.git"),
            "cloning https://github.com/o/r.git"
        );
    }
}
