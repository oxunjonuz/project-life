//! The MCP server as its own process (DOP_TZ §1).
//!
//! A separate binary on purpose. The reading surface an agent is allowed to touch lives in
//! `src/mcp.rs` and `quick.rs`; the writing surface lives in the CLI. Keeping the server out of the
//! CLI binary means "the agent-facing process contains no write path" is a claim about two small
//! files, and `tools/mcp_audit.py` checks it against a list of write symbols.
//!
//! Usage: `pl-mcp --archive <archive-root>` (or `PROJECTLIFE_ARCHIVE`). Speaks JSON-RPC over stdio,
//! newline-delimited, as the MCP stdio transport specifies.

use std::path::PathBuf;

fn help() -> String {
    format!(
        "pl-mcp {} — read-only Project Life server (MCP over stdio)\n\n\
         Usage: pl-mcp --archive <archive-root>\n\n\
         Registered tools: {}\n\
         Write operations are NOT registered and cannot be called through this server.\n",
        projectlife::VERSION,
        projectlife::mcp::tool_names().join(", ")
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--archive" | "-a" => {
                i += 1;
                match args.get(i) {
                    Some(v) => root = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("--archive needs a path");
                        std::process::exit(2);
                    }
                }
            }
            "version" | "--version" | "-V" => {
                println!("projectlife-mcp {}", projectlife::VERSION);
                return;
            }
            "help" | "--help" | "-h" => {
                print!("{}", help());
                return;
            }
            other if other.starts_with("--archive=") => {
                root = Some(PathBuf::from(&other["--archive=".len()..]));
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{}", help());
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let root = match root.or_else(|| std::env::var("PROJECTLIFE_ARCHIVE").ok().map(PathBuf::from)) {
        Some(r) => r,
        None => {
            eprintln!("{}", help());
            std::process::exit(2);
        }
    };
    match projectlife::mcp::serve(&root) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
