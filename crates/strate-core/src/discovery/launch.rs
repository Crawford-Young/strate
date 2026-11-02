//! `claude --bg` launch commands (#108): a Bash or PowerShell command that
//! starts a background session, and the name and resumed session it gives.

/// One `claude --bg` (or `--background`) invocation in a shell command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
    /// `-n X` / `--name X` / `--name=X`.
    pub name: Option<String>,
    /// `-r X` / `--resume X` / `--resume=X`: the session it continues.
    pub resume: Option<String>,
}

/// `claude` subcommands: a command naming one launches no session.
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "attach",
    "config",
    "doctor",
    "install",
    "mcp",
    "migrate-installer",
    "plugin",
    "setup-token",
    "update",
];

/// Words that run the command after them.
const PREFIXES: &[&str] = &["nohup", "exec", "env", "command"];

/// `timeout` options that take a value as the next word.
const TIMEOUT_VALUED: &[&str] = &["-s", "-k", "--signal", "--kill-after"];

/// Every background launch in `command`, in order: each simple command
/// whose program is `claude`, `claude.exe` or `claude.cmd` (bare, or a
/// path ending in one, quoted or not) with a `--bg` or `--background` flag,
/// after any environment assignments and `nohup`, `exec`, `env`, `command`
/// or `timeout [options] <duration>`. Arguments after `--` are not flags.
/// Text in quotes, comments (`#`, PowerShell `<# … #>`) and heredoc or
/// here-string bodies is never a command. A heuristic: `env` and `command`
/// options, `xargs`, `cmd /c`, `Start-Process` and `$( … )` are not read.
pub fn launches(command: &str) -> Vec<Launch> {
    simple_commands(command)
        .iter()
        .filter_map(|words| launch(words))
        .collect()
}

/// Where the program of simple command `words` sits, past its prefixes.
fn program_at(words: &[String]) -> Option<usize> {
    let mut at = 0;
    loop {
        let word = words.get(at)?.as_str();
        if PREFIXES.contains(&word) || is_assignment(word) {
            at += 1;
        } else if word == "timeout" {
            at += 1;
            while let Some(option) = words.get(at).filter(|w| w.starts_with('-')) {
                at += if TIMEOUT_VALUED.contains(&option.as_str()) {
                    2
                } else {
                    1
                };
            }
            // Its duration.
            at += 1;
        } else {
            return Some(at);
        }
    }
}

/// The launch `words` (one simple command) make, if any.
fn launch(words: &[String]) -> Option<Launch> {
    let start = program_at(words)?;
    let program = words[start]
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    if !matches!(program.as_str(), "claude" | "claude.exe" | "claude.cmd") {
        return None;
    }
    let args = &words[start + 1..];
    if args
        .first()
        .is_some_and(|a| SUBCOMMANDS.contains(&a.as_str()))
    {
        return None;
    }
    let mut out = Launch::default();
    let mut background = false;
    let mut at = args.iter();
    while let Some(arg) = at.next() {
        match arg.as_str() {
            "--" => break,
            "--bg" | "--background" => background = true,
            "-n" | "--name" => out.name = at.next().cloned(),
            "-r" | "--resume" => out.resume = at.next().cloned(),
            _ => {
                if let Some(v) = arg.strip_prefix("--name=") {
                    out.name = Some(v.to_string());
                } else if let Some(v) = arg.strip_prefix("--resume=") {
                    out.resume = Some(v.to_string());
                }
            }
        }
    }
    background.then_some(out)
}

/// `NAME=value`: an environment assignment before the program.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with('-')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `command` split into simple commands, each a list of words with their
/// quotes removed. `;`, `&`, `|`, `(`, `)` and newlines separate commands;
/// `#` at a word's start comments to the line's end, and `<#` to the next
/// `#>` (PowerShell's block comment); a heredoc's body
/// (`<<EOF` up to its `EOF` line) and a PowerShell here-string (`@'` up to
/// `'@`) are skipped. Bash quoting, except that an unquoted backslash
/// escapes only whitespace, a quote or a separator, so a Windows path
/// keeps its backslashes.
fn simple_commands(command: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut heredocs: Vec<String> = Vec::new();
    let mut i = 0;
    let end = |out: &mut Vec<Vec<String>>, word: &mut Option<String>| {
        if let Some(w) = word.take() {
            out.last_mut().expect("a command").push(w);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    w.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\'
                        && chars
                            .get(i + 1)
                            .is_some_and(|n| matches!(n, '$' | '`' | '"' | '\\' | '\n'))
                    {
                        i += 1;
                    }
                    w.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => match chars.get(i + 1) {
                Some('\n') => i += 1,
                Some(&n) if n.is_whitespace() || "'\";&|()#".contains(n) => {
                    word.get_or_insert_with(String::new).push(n);
                    i += 1;
                }
                _ => word.get_or_insert_with(String::new).push(c),
            },
            '#' if word.is_none() => {
                while i + 1 < chars.len() && chars[i + 1] != '\n' {
                    i += 1;
                }
            }
            '<' if word.is_none() && chars.get(i + 1) == Some(&'#') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '#' && chars.get(i + 1) == Some(&'>')) {
                    i += 1;
                }
                i += 1;
            }
            '@' if word.is_none() && matches!(chars.get(i + 1), Some('\'' | '"')) => {
                let close = [chars[i + 1], '@'];
                i += 2;
                while i < chars.len()
                    && !(chars[i] == '\n' && chars.get(i + 1..i + 3) == Some(&close[..]))
                {
                    i += 1;
                }
                i += 2;
                word = Some(String::new());
            }
            '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') => {
                end(&mut out, &mut word);
                i += 2;
                if chars.get(i) == Some(&'-') {
                    i += 1;
                }
                while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
                    i += 1;
                }
                let mut delimiter = String::new();
                while let Some(&c) = chars.get(i) {
                    if c.is_whitespace() || ";&|<>()".contains(c) {
                        break;
                    }
                    if c != '\'' && c != '"' {
                        delimiter.push(c);
                    }
                    i += 1;
                }
                heredocs.push(delimiter);
                continue;
            }
            '\n' => {
                end(&mut out, &mut word);
                out.push(Vec::new());
                for delimiter in heredocs.drain(..) {
                    loop {
                        let start = i + 1;
                        let stop = chars[start.min(chars.len())..]
                            .iter()
                            .position(|c| *c == '\n')
                            .map_or(chars.len(), |p| start + p);
                        let line: String = chars[start.min(stop)..stop].iter().collect();
                        i = stop;
                        if line.trim() == delimiter || stop >= chars.len() {
                            break;
                        }
                    }
                }
            }
            ';' | '&' | '|' | '(' | ')' => {
                end(&mut out, &mut word);
                out.push(Vec::new());
            }
            c if c.is_whitespace() => end(&mut out, &mut word),
            c => word.get_or_insert_with(String::new).push(c),
        }
        i += 1;
    }
    end(&mut out, &mut word);
    out.retain(|words| !words.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Launch {
        Launch {
            name: Some(name.into()),
            resume: None,
        }
    }

    #[test]
    fn a_claude_bg_command_launches_with_its_name_and_resume() {
        assert_eq!(
            launches("claude --bg -n impl_issue_x-1"),
            [named("impl_issue_x-1")]
        );
        assert_eq!(
            launches("claude --background --name lorem"),
            [named("lorem")]
        );
        assert_eq!(
            launches("claude --name=lorem --bg 'do it'"),
            [named("lorem")]
        );
        assert_eq!(
            launches(r#"claude --bg --name "lorem ipsum" "Lorem prompt.""#),
            [named("lorem ipsum")]
        );
        assert_eq!(
            launches("claude --bg --resume 5e55-0001"),
            [Launch {
                name: None,
                resume: Some("5e55-0001".into())
            }]
        );
        assert_eq!(
            launches("claude -r 5e55-0001 --bg -n x"),
            [Launch {
                name: Some("x".into()),
                resume: Some("5e55-0001".into())
            }]
        );
        assert_eq!(launches("claude --bg"), [Launch::default()]);
    }

    #[test]
    fn the_program_may_be_a_path_or_exe_and_follow_other_commands() {
        assert_eq!(
            launches(r#"& "C:\Users\x\.local\bin\claude.exe" --bg -n a"#),
            [named("a")]
        );
        assert_eq!(
            launches("/home/x/.local/bin/claude --bg -n a"),
            [named("a")]
        );
        assert_eq!(launches(r"C:\x\Claude.exe --bg -n a"), [named("a")]);
        assert_eq!(
            launches("cd /work/x && FOO=1 claude --bg -n a; claude --bg -n b | cat"),
            [named("a"), named("b")]
        );
        assert_eq!(launches("nohup claude --bg -n a &"), [named("a")]);
        assert_eq!(launches("env FOO=1 claude --bg -n a"), [named("a")]);
        assert_eq!(launches("command claude --bg -n a"), [named("a")]);
        assert_eq!(launches("claude.cmd --bg -n a"), [named("a")]);
        assert_eq!(launches(r"C:\x\claude.CMD --bg -n a"), [named("a")]);
        for command in [
            "timeout 600 claude --bg -n a",
            "timeout -s KILL 10m claude --bg -n a",
            "timeout --signal=KILL -k 5 1h claude --bg -n a",
        ] {
            assert_eq!(launches(command), [named("a")], "{command:?}");
        }
        assert_eq!(
            launches("echo start\nclaude \\\n  --bg -n a"),
            [named("a")],
            "a continued line"
        );
    }

    #[test]
    fn claude_in_other_words_strings_comments_and_heredocs_is_no_launch() {
        for command in [
            "claude -n a",
            "claude agents --bg",
            "claude attach abc --bg",
            "myclaude --bg -n a",
            "claude-code --bg -n a",
            "grep 'claude --bg' notes.txt",
            "echo \"claude --bg -n a\"",
            "echo claude --bg -n a",
            "rg -n claude --bg",
            "# claude --bg -n a",
            "ls # claude --bg",
            "which claude",
            "cat <<'EOF'\nclaude --bg -n a\nEOF",
            "cat <<-EOF > x.md\n\tclaude --bg -n a\n\tEOF\necho done",
            "$x = @'\nclaude --bg -n a\n'@",
            "git commit -m \"run claude --bg\"",
            "claude --bgx",
            "claude -- --bg",
            "timeout claude --bg -n a",
            "command -v claude --bg",
            "<# claude --bg -n a #>",
            "<#\nclaude --bg -n a\n#>",
        ] {
            assert_eq!(launches(command), [], "{command:?}");
        }
        assert_eq!(
            launches("cat <<EOF\nclaude --bg -n a\nEOF\nclaude --bg -n b"),
            [named("b")],
            "the command after a heredoc still counts"
        );
        assert_eq!(
            launches("<# lorem\nipsum #> claude --bg -n b"),
            [named("b")],
            "the command after a block comment still counts"
        );
    }
}
