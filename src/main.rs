use std::{
    env, fs,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use git_rust::{
    objects::{
        GitObjectKind, initialize_repository, parse_tree_entries, read_git_object,
        split_header_and_body, validate_sha1_hex, write_git_object, write_working_tree,
    },
    packfile::clone_repository,
};
use hex::encode;

fn infer_target_directory(repo_url: &str) -> String {
    let trimmed = repo_url.trim_end_matches('/');
    let repo_segment = trimmed.rsplit('/').next().unwrap_or("repo");
    let clean_name = repo_segment.strip_suffix(".git").unwrap_or(repo_segment);
    if clean_name.is_empty() {
        "repo".to_string()
    } else {
        clean_name.to_string()
    }
}

fn print_help() {
    eprintln!("git-rust - A from-scratch Git client written in Rust");
    eprintln!();
    eprintln!("Usage: git-rust <command> [options]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!(
        "  init                                              Initialize a new .git directory"
    );
    eprintln!(
        "  cat-file -p <object-sha>                          Pretty-print a Git object's contents"
    );
    eprintln!(
        "  hash-object [-w] <file>                           Compute SHA-1 (and optionally store blob)"
    );
    eprintln!("  ls-tree [--name-only] <tree-sha>                  List entries in a tree object");
    eprintln!(
        "  write-tree                                        Write the working directory as a tree object"
    );
    eprintln!("  commit-tree <tree-sha> [-p <parent>] -m <message> Create a new commit object");
    eprintln!(
        "  clone <repo-url> [target-dir]                     Clone a public HTTP(S) repository"
    );
}

fn run_cli() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        bail!("No subcommand provided");
    }

    let git_dir = Path::new(".git");

    match args[1].as_str() {
        "init" => {
            initialize_repository(Path::new("."), "refs/heads/main")?;
            println!("Initialized git directory");
        }
        "cat-file" => {
            if !args.iter().any(|arg| arg == "-p") {
                bail!("Usage: git-rust cat-file -p <object-sha>");
            }
            let object_sha = args
                .last()
                .filter(|arg| *arg != "-p" && *arg != "cat-file")
                .context("Missing <object-sha> argument")?;

            let raw_object = read_git_object(git_dir, object_sha)?;
            let (_, body) = split_header_and_body(&raw_object)?;
            std::io::stdout().write_all(body)?;
        }
        "hash-object" => {
            let persist_to_disk = args.iter().any(|arg| arg == "-w");
            let file_path = args
                .last()
                .filter(|arg| *arg != "-w" && *arg != "hash-object")
                .context("Usage: git-rust hash-object [-w] <file>")?;

            let file_bytes = fs::read(file_path)
                .with_context(|| format!("Failed to read file '{}'", file_path))?;
            let sha_bytes =
                write_git_object(git_dir, GitObjectKind::Blob, &file_bytes, persist_to_disk)?;
            println!("{}", encode(sha_bytes));
        }
        "ls-tree" => {
            let tree_sha = args
                .last()
                .filter(|arg| !arg.starts_with('-') && *arg != "ls-tree")
                .context("Usage: git-rust ls-tree [--name-only] <tree-sha>")?;

            let raw_tree = read_git_object(git_dir, tree_sha)?;
            let (_, tree_body) = split_header_and_body(&raw_tree)?;
            let entries = parse_tree_entries(tree_body)?;

            for entry in &entries {
                println!("{}", entry.name);
            }
        }
        "write-tree" => {
            let root_tree = write_working_tree(git_dir, Path::new("."))?
                .context("Working directory is empty; nothing to write")?;
            println!("{}", encode(root_tree.sha));
        }
        "commit-tree" => {
            let tree_sha = args
                .get(2)
                .context("Usage: git-rust commit-tree <tree-sha> [-p <parent-sha>] -m <message>")?;
            validate_sha1_hex(tree_sha)?;

            let author_name =
                env::var("GIT_AUTHOR_NAME").unwrap_or_else(|_| "John Doe".to_string());
            let author_email =
                env::var("GIT_AUTHOR_EMAIL").unwrap_or_else(|_| "john.doe@example.com".to_string());
            let unix_seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let timestamp = format!("{} +0000", unix_seconds);

            let mut flag_iter = args[3..].iter();
            let mut parent_sha: Option<&String> = None;
            let mut commit_message: Option<&String> = None;

            while let Some(flag) = flag_iter.next() {
                match flag.as_str() {
                    "-p" => parent_sha = flag_iter.next(),
                    "-m" => commit_message = flag_iter.next(),
                    _ => {}
                }
            }

            let message = commit_message.context("Missing commit message (-m <message>)")?;

            let mut commit_body = String::new();
            commit_body.push_str(&format!("tree {}\n", tree_sha));
            if let Some(parent) = parent_sha {
                validate_sha1_hex(parent)?;
                commit_body.push_str(&format!("parent {}\n", parent));
            }
            commit_body.push_str(&format!(
                "author {} <{}> {}\n",
                author_name, author_email, timestamp
            ));
            commit_body.push_str(&format!(
                "committer {} <{}> {}\n\n",
                author_name, author_email, timestamp
            ));
            commit_body.push_str(&format!("{}\n", message));

            let commit_sha =
                write_git_object(git_dir, GitObjectKind::Commit, commit_body.as_bytes(), true)?;
            println!("{}", encode(commit_sha));
        }
        "clone" => {
            let repo_url = args
                .get(2)
                .context("Usage: git-rust clone <repo-url> [target-dir]")?;
            let target_dir_name = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| infer_target_directory(repo_url));

            clone_repository(repo_url, Path::new(&target_dir_name))?;
        }
        unknown => {
            print_help();
            bail!("Unknown command: '{}'", unknown);
        }
    }

    Ok(())
}

fn main() {
    if let Err(error) = run_cli() {
        eprintln!("error: {:#}", error);
        std::process::exit(1);
    }
}
