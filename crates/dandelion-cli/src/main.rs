mod acp_stdio;
mod app;
mod cli;
mod render;
mod serve;

#[tokio::main]
async fn main() {
    if let Err(err) = app::run().await {
        eprintln!("dln: {err}");
        // H5 eval contract: 2 = nothing ran (bad usage / unreadable input /
        // missing tooling), 1 = the command ran and failed, incl. eval cases.
        if err.downcast_ref::<app::EvalUsageError>().is_some() {
            std::process::exit(2);
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::{
        Cli, Command, CorpusCommand, CronCommand, McpCommand, MemoryCommand, SessionCommand,
        SkillsCommand, SpecCommand,
    };
    use clap::Parser;

    #[test]
    fn cli_must_not_depend_on_reqwest() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("reqwest"),
            "dandelion-cli must not talk to models; keep reqwest in dandelion-core"
        );
    }

    #[test]
    fn cli_must_not_depend_on_rmcp() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("rmcp"),
            "dandelion-cli must not spawn MCP; keep rmcp in dandelion-core"
        );
    }

    #[test]
    fn parses_g2_subcommands_and_flags() {
        let skills = Cli::try_parse_from(["dln", "skills", "list"]).unwrap();
        assert!(matches!(
            skills.command,
            Command::Skills {
                command: SkillsCommand::List
            }
        ));
        let imported = Cli::try_parse_from(["dln", "skills", "import", "pack/SKILL.md"]).unwrap();
        match imported.command {
            Command::Skills {
                command: SkillsCommand::Import { path },
            } => assert!(path.ends_with("SKILL.md"), "{path:?}"),
            other => panic!("{other:?}"),
        }
        let listed = Cli::try_parse_from(["dln", "mcp", "list"]).unwrap();
        assert!(matches!(
            listed.command,
            Command::Mcp {
                command: McpCommand::List
            }
        ));
        let start = Cli::try_parse_from(["dln", "mcp", "start", "echo"]).unwrap();
        match start.command {
            Command::Mcp {
                command: McpCommand::Start { id },
            } => assert_eq!(id, "echo"),
            other => panic!("{other:?}"),
        }
        let mcp = Cli::try_parse_from(["dln", "mcp", "stop", "echo"]).unwrap();
        match mcp.command {
            Command::Mcp {
                command: McpCommand::Stop { id },
            } => assert_eq!(id, "echo"),
            other => panic!("{other:?}"),
        }
        // C18 install / uninstall / registry
        let removed = Cli::try_parse_from(["dln", "skills", "remove", "demo-skill"]).unwrap();
        match removed.command {
            Command::Skills {
                command: SkillsCommand::Remove { id },
            } => assert_eq!(id, "demo-skill"),
            other => panic!("{other:?}"),
        }
        let install = Cli::try_parse_from([
            "dln",
            "skills",
            "install",
            "https://github.com/o/r",
            "--subdir",
            "skills",
        ])
        .unwrap();
        match install.command {
            Command::Skills {
                command: SkillsCommand::Install { url, subdir, git },
            } => {
                assert_eq!(url, "https://github.com/o/r");
                assert_eq!(subdir.as_deref(), Some("skills"));
                assert!(!git);
            }
            other => panic!("{other:?}"),
        }
        let search = Cli::try_parse_from(["dln", "mcp", "search", "files"]).unwrap();
        match search.command {
            Command::Mcp {
                command: McpCommand::Search { query, cursor },
            } => {
                assert_eq!(query, "files");
                assert!(cursor.is_none());
            }
            other => panic!("{other:?}"),
        }
        let mcp_install = Cli::try_parse_from([
            "dln",
            "mcp",
            "install",
            "io.github.o/s",
            "--version",
            "1.2.3",
        ])
        .unwrap();
        match mcp_install.command {
            Command::Mcp {
                command: McpCommand::Install { name, version },
            } => {
                assert_eq!(name, "io.github.o/s");
                assert_eq!(version.as_deref(), Some("1.2.3"));
            }
            other => panic!("{other:?}"),
        }
        let mcp_remove = Cli::try_parse_from(["dln", "mcp", "remove", "echo"]).unwrap();
        match mcp_remove.command {
            Command::Mcp {
                command: McpCommand::Remove { id },
            } => assert_eq!(id, "echo"),
            other => panic!("{other:?}"),
        }
        let chat = Cli::try_parse_from([
            "dln",
            "chat",
            "--permission-mode",
            "auto",
            "-s",
            "demo-skill",
            "--mcp",
            "echo",
            "-q",
            "hi",
        ])
        .unwrap();
        match chat.command {
            Command::Chat {
                prompt,
                skill,
                mcp,
                permission_mode,
                ..
            } => {
                assert_eq!(prompt.as_deref(), Some("hi"));
                assert_eq!(skill, vec!["demo-skill"]);
                assert_eq!(mcp, vec!["echo"]);
                assert_eq!(permission_mode.as_deref(), Some("auto"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_g3_subcommands() {
        let s = Cli::try_parse_from(["dln", "session", "list", "--limit", "20"]).unwrap();
        match s.command {
            Command::Session {
                command: SessionCommand::List { limit, offset },
            } => {
                assert_eq!(limit, Some(20));
                assert_eq!(offset, 0);
            }
            other => panic!("{other:?}"),
        }
        let exp = Cli::try_parse_from(["dln", "session", "export", "tsk_1"]).unwrap();
        match exp.command {
            Command::Session {
                command: SessionCommand::Export { id },
            } => assert_eq!(id, "tsk_1"),
            other => panic!("{other:?}"),
        }
        let search = Cli::try_parse_from(["dln", "session", "search", "needle"]).unwrap();
        match search.command {
            Command::Session {
                command: SessionCommand::Search { query },
            } => assert_eq!(query, "needle"),
            other => panic!("{other:?}"),
        }
        let title =
            Cli::try_parse_from(["dln", "session", "set-title", "tsk_1", "new name"]).unwrap();
        match title.command {
            Command::Session {
                command: SessionCommand::SetTitle { id, title },
            } => {
                assert_eq!(id, "tsk_1");
                assert_eq!(title, "new name");
            }
            other => panic!("{other:?}"),
        }
        let del = Cli::try_parse_from(["dln", "session", "delete", "tsk_1"]).unwrap();
        match del.command {
            Command::Session {
                command: SessionCommand::Delete { id },
            } => assert_eq!(id, "tsk_1"),
            other => panic!("{other:?}"),
        }
        let dup = Cli::try_parse_from(["dln", "session", "duplicate", "tsk_1"]).unwrap();
        match dup.command {
            Command::Session {
                command: SessionCommand::Duplicate { id },
            } => assert_eq!(id, "tsk_1"),
            other => panic!("{other:?}"),
        }
        let occ = Cli::try_parse_from(["dln", "session", "occupy", "tsk_1", "--surface", "main"])
            .unwrap();
        match occ.command {
            Command::Session {
                command: SessionCommand::Occupy { id, surface },
            } => {
                assert_eq!(id, "tsk_1");
                assert_eq!(surface, "main");
            }
            other => panic!("{other:?}"),
        }
        let prefs = Cli::try_parse_from([
            "dln",
            "session",
            "set-prefs",
            "tsk_1",
            "--temperature",
            "0.3",
        ])
        .unwrap();
        match prefs.command {
            Command::Session {
                command: SessionCommand::SetPrefs { temperature, .. },
            } => assert_eq!(temperature, Some(0.3)),
            other => panic!("{other:?}"),
        }
        let c = Cli::try_parse_from([
            "dln",
            "cron",
            "add",
            "--expr",
            "30m",
            "--prompt",
            "ping",
            "--missed-policy",
            "run_once",
        ])
        .unwrap();
        match c.command {
            Command::Cron {
                command:
                    CronCommand::Add {
                        expr,
                        prompt,
                        missed_policy,
                        ..
                    },
            } => {
                assert_eq!(expr, "30m");
                assert_eq!(prompt, "ping");
                assert_eq!(missed_policy, "run_once");
            }
            other => panic!("{other:?}"),
        }
        for (args, expect) in [
            (["dln", "cron", "pause", "cron_1"].as_slice(), "pause"),
            (["dln", "cron", "resume", "cron_1"].as_slice(), "resume"),
            (["dln", "cron", "run", "cron_1"].as_slice(), "run"),
            (["dln", "cron", "remove", "cron_1"].as_slice(), "remove"),
        ] {
            let parsed = Cli::try_parse_from(args.iter().copied()).unwrap();
            match (expect, parsed.command) {
                (
                    "pause",
                    Command::Cron {
                        command: CronCommand::Pause { id },
                    },
                ) => assert_eq!(id, "cron_1"),
                (
                    "resume",
                    Command::Cron {
                        command: CronCommand::Resume { id },
                    },
                ) => assert_eq!(id, "cron_1"),
                (
                    "run",
                    Command::Cron {
                        command: CronCommand::Run { id },
                    },
                ) => assert_eq!(id, "cron_1"),
                (
                    "remove",
                    Command::Cron {
                        command: CronCommand::Remove { id },
                    },
                ) => assert_eq!(id, "cron_1"),
                other => panic!("{other:?}"),
            }
        }
        let m = Cli::try_parse_from(["dln", "memory", "show"]).unwrap();
        assert!(matches!(
            m.command,
            Command::Memory {
                command: MemoryCommand::Show
            }
        ));
        let set = Cli::try_parse_from(["dln", "memory", "set", "soul", "--file", "-"]).unwrap();
        match set.command {
            Command::Memory {
                command: MemoryCommand::Set { kind, file },
            } => {
                assert_eq!(kind, "soul");
                assert_eq!(file, "-");
            }
            other => panic!("{other:?}"),
        }
        let c = Cli::try_parse_from(["dln", "corpus", "list"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Corpus {
                command: CorpusCommand::List
            }
        ));
        let add = Cli::try_parse_from(["dln", "corpus", "add", "notes.txt"]).unwrap();
        match add.command {
            Command::Corpus {
                command: CorpusCommand::Add { path },
            } => assert!(path.ends_with("notes.txt")),
            other => panic!("{other:?}"),
        }
        let compact = Cli::try_parse_from(["dln", "compact", "-c", "tsk_abc"]).unwrap();
        match compact.command {
            Command::Compact { session } => assert_eq!(session, "tsk_abc"),
            other => panic!("{other:?}"),
        }
        let ask = Cli::try_parse_from([
            "dln",
            "spec",
            "ask",
            "-q",
            "add doctor line",
            "--permission-mode",
            "auto",
        ])
        .unwrap();
        match ask.command {
            Command::Spec {
                command:
                    SpecCommand::Ask {
                        prompt,
                        permission_mode,
                        ..
                    },
            } => {
                assert_eq!(prompt, "add doctor line");
                assert_eq!(permission_mode.as_deref(), Some("auto"));
            }
            other => panic!("{other:?}"),
        }
        let revise =
            Cli::try_parse_from(["dln", "spec", "revise", "-c", "tsk_1", "-q", "add CI"]).unwrap();
        match revise.command {
            Command::Spec {
                command:
                    SpecCommand::Revise {
                        session, prompt, ..
                    },
            } => {
                assert_eq!(session, "tsk_1");
                assert_eq!(prompt, "add CI");
            }
            other => panic!("{other:?}"),
        }
        let show = Cli::try_parse_from(["dln", "spec", "show", "-c", "tsk_1"]).unwrap();
        assert!(matches!(
            show.command,
            Command::Spec {
                command: SpecCommand::Show { .. }
            }
        ));
        let wiki = Cli::try_parse_from(["dln", "wiki", "status"]).unwrap();
        assert!(matches!(
            wiki.command,
            Command::Wiki {
                command: crate::cli::WikiCommand::Status
            }
        ));
        let compile = Cli::try_parse_from(["dln", "wiki", "compile", "--workspace", "."]).unwrap();
        match compile.command {
            Command::Wiki {
                command: crate::cli::WikiCommand::Compile { workspace, .. },
            } => assert!(workspace.is_some()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            Cli::try_parse_from(["dln", "acp"]).unwrap().command,
            Command::Acp
        ));
        // H5 eval surface: run streams JSONL with a suite selector; compare
        // takes exactly two report paths.
        let run = Cli::try_parse_from(["dln", "eval", "run", "--jsonl", "out.jsonl"]).unwrap();
        match run.command {
            Command::Eval {
                command: crate::cli::EvalCommand::Run { suite, jsonl },
            } => {
                assert_eq!(suite, "smoke");
                assert!(jsonl.ends_with("out.jsonl"), "{jsonl:?}");
            }
            other => panic!("{other:?}"),
        }
        let cmp = Cli::try_parse_from(["dln", "eval", "compare", "a.jsonl", "b.jsonl"]).unwrap();
        match cmp.command {
            Command::Eval {
                command:
                    crate::cli::EvalCommand::Compare {
                        baseline,
                        candidate,
                    },
            } => {
                assert!(baseline.ends_with("a.jsonl"));
                assert!(candidate.ends_with("b.jsonl"));
            }
            other => panic!("{other:?}"),
        }
        // Bad compare input must surface the exit-2 sentinel, not a silent 0.
        let missing = crate::app::eval_compare_for_test("no-such-a.jsonl", "no-such-b.jsonl");
        assert!(missing
            .err()
            .is_some_and(|err| err.is::<crate::app::EvalUsageError>()));
        match Cli::try_parse_from(["dln", "serve", "--bind", "127.0.0.1:3927"])
            .unwrap()
            .command
        {
            Command::Serve { bind } => assert_eq!(bind.as_deref(), Some("127.0.0.1:3927")),
            other => panic!("{other:?}"),
        }
    }
}
